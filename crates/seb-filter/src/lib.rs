use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(i32)]
pub enum FilterDecision {
    Allow = 0,
    Block = 1,
    NoMatch = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(i32)]
pub enum ResourceType {
    MainFrame = 0,
    NavigationPreloadMainFrame = 1,
    SubFrame = 2,
    Other = 3,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilterRuleConfig {
    pub active: bool,
    pub expression: String,
    /// true = Regex, false = Simplified
    pub is_regex: bool,
    /// true = Allow, false = Block
    pub allow: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilterConfig {
    pub process_main_requests: bool,
    pub process_content_requests: bool,
    pub rules: Vec<FilterRuleConfig>,
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let h1 = (bytes[i + 1] as char).to_digit(16);
            let h2 = (bytes[i + 2] as char).to_digit(16);
            if let (Some(a), Some(b)) = (h1, h2) {
                out.push(((a << 4) | b) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn build_ci_anchored(expr: &str) -> Option<Regex> {
    RegexBuilder::new(&format!("^({expr})$"))
        .case_insensitive(true)
        .build()
        .ok()
}

fn replace_wildcard(escaped: String) -> String {
    escaped.replace("\\*", ".*")
}

struct CompiledRegexRule {
    re: Regex,
}

impl CompiledRegexRule {
    fn compile(expression: &str) -> Option<Self> {
        let re = RegexBuilder::new(expression)
            .case_insensitive(true)
            .build()
            .ok()?;
        Some(Self { re })
    }

    fn is_match(&self, raw_url: &str) -> bool {
        let decoded = percent_decode(raw_url);
        self.re.is_match(&decoded)
    }
}

struct CompiledSimplifiedRule {
    scheme: Option<Regex>,
    user_info: Option<Regex>,
    host: Regex,
    port: Option<u16>,
    path: Option<Regex>,
    query: Option<Regex>,
    fragment: Option<Regex>,
}

impl CompiledSimplifiedRule {
    fn compile(expression: &str) -> Option<Self> {
        let allowed = Regex::new(r"[a-zA-Z0-9\*]+").ok()?;
        if !allowed.is_match(expression) {
            return None;
        }
        let delim = Regex::new(
            r"^(?:([^:/\?#]*)://)?(?:([^@]*?)(?::([^@]*))?@)?(?:([^/:\?#]*))?(?::([0-9\*]*))?([^\?#]*)?(?:\?([^#]*))?(?:#(.*))?$",
        )
        .ok()?;
        let caps = delim.captures(expression)?;

        let scheme = match caps.get(1).map(|m| m.as_str()).filter(|s| !s.is_empty()) {
            Some(s) => Some(build_ci_anchored(&replace_wildcard(regex::escape(s)))?),
            None => None,
        };

        let user_info = match caps.get(2).map(|m| m.as_str()).filter(|s| !s.is_empty()) {
            Some(user) => {
                let mut ui = regex::escape(user);
                let pass = caps.get(3).map(|m| m.as_str()).unwrap_or("");
                if pass.is_empty() {
                    ui.push_str("(:.*)?");
                } else {
                    ui.push(':');
                    ui.push_str(&regex::escape(pass));
                }
                Some(build_ci_anchored(&replace_wildcard(ui))?)
            }
            None => None,
        };

        let mut host_raw = caps.get(4).map(|m| m.as_str()).unwrap_or("").to_string();
        let alnum = Regex::new(r"^[a-zA-Z0-9]+$").ok()?;
        let alphanumeric = alnum.is_match(&host_raw);
        let exact_subdomain = host_raw.starts_with('.');
        if exact_subdomain {
            host_raw.remove(0);
        }
        let mut host_pattern = replace_wildcard(regex::escape(&host_raw));
        if !alphanumeric && !exact_subdomain {
            host_pattern = format!("(.+?\\.)*{host_pattern}");
        }
        let host = build_ci_anchored(&host_pattern)?;

        let port = caps
            .get(5)
            .map(|m| m.as_str())
            .and_then(|s| s.parse::<u16>().ok());

        let path_raw = caps.get(6).map(|m| m.as_str()).unwrap_or("");
        let path = if !path_raw.trim().is_empty() && path_raw != "/" {
            let mut p = replace_wildcard(regex::escape(path_raw));
            if p.ends_with('/') {
                p.push('?');
            } else {
                p.push_str("/?");
            }
            Some(build_ci_anchored(&p)?)
        } else {
            None
        };

        let query_raw = caps.get(7).map(|m| m.as_str()).unwrap_or("");
        let query = if !query_raw.trim().is_empty() {
            let q = if query_raw == "." {
                "\\??".to_string()
            } else {
                format!("\\??{}", replace_wildcard(regex::escape(query_raw)))
            };
            Some(build_ci_anchored(&q)?)
        } else {
            None
        };

        let frag_raw = caps.get(8).map(|m| m.as_str()).unwrap_or("");
        let fragment = if !frag_raw.trim().is_empty() {
            let f = format!("#?{}", replace_wildcard(regex::escape(frag_raw)));
            Some(build_ci_anchored(&f)?)
        } else {
            None
        };

        Some(Self {
            scheme,
            user_info,
            host,
            port,
            path,
            query,
            fragment,
        })
    }

    fn is_match(&self, parsed: &Url) -> bool {
        let scheme = parsed.scheme();
        let user_info = if !parsed.username().is_empty() {
            match parsed.password() {
                Some(p) => format!("{}:{}", parsed.username(), p),
                None => parsed.username().to_string(),
            }
        } else {
            String::new()
        };
        let host = percent_decode(parsed.host_str().unwrap_or(""));
        let path = percent_decode(parsed.path());
        let query = match parsed.query() {
            Some(q) if !q.is_empty() => format!("?{}", percent_decode(q)),
            _ => String::new(),
        };
        let fragment = match parsed.fragment() {
            Some(f) if !f.is_empty() => format!("#{}", percent_decode(f)),
            _ => String::new(),
        };

        let effective_port = parsed.port_or_known_default();

        let mut matched = true;
        matched &= self.scheme.as_ref().map_or(true, |re| re.is_match(scheme));
        matched &= self
            .user_info
            .as_ref()
            .map_or(true, |re| re.is_match(&user_info));
        matched &= self.host.is_match(&host);
        matched &= self
            .port
            .map_or(true, |p| effective_port == Some(p));
        matched &= self.path.as_ref().map_or(true, |re| re.is_match(&path));
        matched &= self.query.as_ref().map_or(true, |re| re.is_match(&query));
        matched &= self
            .fragment
            .as_ref()
            .map_or(true, |re| re.is_match(&fragment));
        matched
    }
}

enum CompiledRule {
    Regex(CompiledRegexRule),
    Simplified(CompiledSimplifiedRule),
}

impl CompiledRule {
    fn is_match(&self, raw_url: &str, parsed: Option<&Url>) -> bool {
        match self {
            CompiledRule::Regex(r) => r.is_match(raw_url),
            CompiledRule::Simplified(s) => parsed.map_or(false, |u| s.is_match(u)),
        }
    }
}

pub struct RequestFilter {
    process_main: bool,
    process_content: bool,
    allow_rules: Vec<CompiledRule>,
    block_rules: Vec<CompiledRule>,
}

impl RequestFilter {
    pub fn new(config: &FilterConfig) -> Self {
        let mut allow_rules = Vec::new();
        let mut block_rules = Vec::new();

        for rule in &config.rules {
            if !rule.active {
                continue;
            }
            let compiled = if rule.is_regex {
                CompiledRegexRule::compile(&rule.expression).map(CompiledRule::Regex)
            } else {
                CompiledSimplifiedRule::compile(&rule.expression).map(CompiledRule::Simplified)
            };
            if let Some(c) = compiled {
                if rule.allow {
                    allow_rules.push(c);
                } else {
                    block_rules.push(c);
                }
            }
        }

        Self {
            process_main: config.process_main_requests,
            process_content: config.process_content_requests,
            allow_rules,
            block_rules,
        }
    }

    pub fn evaluate(&self, raw_url: &str, resource_type: ResourceType) -> FilterDecision {
        let is_main = matches!(
            resource_type,
            ResourceType::MainFrame | ResourceType::NavigationPreloadMainFrame
        );
        if (is_main && !self.process_main) || (!is_main && !self.process_content) {
            return FilterDecision::NoMatch;
        }

        let parsed = Url::parse(raw_url).ok();
        if is_main {
            if let Some(u) = &parsed {
                if u.scheme().eq_ignore_ascii_case("chrome-extension") {
                    return FilterDecision::NoMatch;
                }
            }
        }

        for rule in &self.block_rules {
            if rule.is_match(raw_url, parsed.as_ref()) {
                return FilterDecision::Block;
            }
        }
        for rule in &self.allow_rules {
            if rule.is_match(raw_url, parsed.as_ref()) {
                return FilterDecision::Allow;
            }
        }
        FilterDecision::Block
    }
}

pub fn is_configuration_resource(raw_url: &str, suggested_file_name: &str, mime_type: &str) -> bool {
    let parsed = Url::parse(raw_url).ok();
    let scheme = parsed
        .as_ref()
        .map(|u| u.scheme().to_ascii_lowercase())
        .unwrap_or_default();
    let decoded_path = parsed
        .as_ref()
        .map(|u| percent_decode(u.path()))
        .unwrap_or_default()
        .to_ascii_lowercase();

    let mime_base = mime_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let is_download_context = !suggested_file_name.is_empty() || !mime_base.is_empty();

    let scheme_match = scheme == "seb" || scheme == "sebs";
    let file_name_match = suggested_file_name.to_ascii_lowercase().ends_with(".seb");
    let path_match = decoded_path.ends_with(".seb")
        || (is_download_context && decoded_path.ends_with("/accessrule/seb/config.php"));
    let mime_match = mime_base == "application/seb" || mime_base == "application/x-seb";
    let data_match = percent_decode(raw_url)
        .to_ascii_lowercase()
        .contains("application/seb");

    scheme_match || file_name_match || path_match || mime_match || data_match
}

pub fn download_url(raw_url: &str) -> String {
    let lower = raw_url.to_ascii_lowercase();
    let is_data_config = lower.contains("application/seb");
    if is_data_config && (lower.starts_with("seb://") || lower.starts_with("sebs://")) {
        if let Some(sep) = raw_url.find("://") {
            return format!("data:{}", &raw_url[sep + 3..]);
        }
    }

    if let Some(rest) = raw_url.strip_prefix("sebs://").or_else(|| {
        if lower.starts_with("sebs://") {
            Some(&raw_url[7..])
        } else {
            None
        }
    }) {
        return format!("https://{rest}");
    }
    if let Some(rest) = raw_url.strip_prefix("seb://").or_else(|| {
        if lower.starts_with("seb://") {
            Some(&raw_url[6..])
        } else {
            None
        }
    }) {
        return format!("http://{rest}");
    }
    raw_url.to_string()
}

pub fn compile_wildcard_regex(pattern: &str) -> Result<Regex, regex::Error> {
    RegexBuilder::new(pattern).case_insensitive(true).build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simplified_rule_matches_subdomains_and_blocks_others() {
        let cfg = FilterConfig {
            process_main_requests: true,
            process_content_requests: true,
            rules: vec![
                FilterRuleConfig {
                    active: true,
                    expression: "lms.example.edu/*".to_string(),
                    is_regex: false,
                    allow: true,
                },
                FilterRuleConfig {
                    active: true,
                    expression: "*://tracker.example/*".to_string(),
                    is_regex: false,
                    allow: false,
                },
            ],
        };
        let filter = RequestFilter::new(&cfg);
        assert_eq!(
            filter.evaluate(
                "https://lms.example.edu/mod/quiz/view.php?id=1",
                ResourceType::MainFrame
            ),
            FilterDecision::Allow
        );
        assert_eq!(
            filter.evaluate("https://tracker.example/pixel.js", ResourceType::Other),
            FilterDecision::Block
        );
        assert_eq!(
            filter.evaluate("https://google.com/", ResourceType::MainFrame),
            FilterDecision::Block
        );
    }

    #[test]
    fn moodle_config_resource_detection_and_url_normalization() {
        assert!(is_configuration_resource(
            "sebs://lms.example.edu/mod/quiz/accessrule/seb/config.php?cmid=12345",
            "",
            ""
        ));
        assert!(is_configuration_resource(
            "https://lms.example.edu/mod/quiz/accessrule/seb/config.php?cmid=12345",
            "config (1).seb",
            "application/seb; charset=utf-8"
        ));
        assert!(!is_configuration_resource(
            "https://lms.example.edu/mod/quiz/accessrule/seb/config.php?cmid=12345",
            "",
            ""
        ));
        assert_eq!(
            download_url("sebs://lms.example.edu/mod/quiz/config.php"),
            "https://lms.example.edu/mod/quiz/config.php"
        );
    }
}
