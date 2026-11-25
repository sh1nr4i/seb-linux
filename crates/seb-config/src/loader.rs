use crate::crypto_pk::CertificateStore;
use crate::plist::{is_xml_plist, parse_xml_plist, serialize_binary_password, serialize_binary_plain};
use crate::settings::*;
use crate::validator::validate_plist_data;
use crate::{
    gzip_decompress, gzip_is_compressed, gzip_peek, rncryptor_decrypt, serialize_canonical_json,
    sha256_hex, DecryptStatus, SebValue,
};
use base64::Engine;
use seb_filter::FilterRuleConfig;
use seb_security::VersionRestriction;
use std::fs;
use std::io::Read;
use std::path::Path;
use url::Url;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadStatus {
    Success,
    PasswordNeeded,
    InvalidData,
    NotSupported,
    LoadWithBrowser,
    UnexpectedError,
}

#[derive(Debug, Clone)]
pub struct LoadedConfiguration {
    pub status: LoadStatus,
    pub settings: AppSettings,
    pub parsed_plist: Option<SebValue>,
    pub raw_seb_data: Option<Vec<u8>>,
    pub used_password_hash: Option<String>,
    pub redirect_url: Option<String>,
    pub message: String,
}

impl LoadedConfiguration {
    fn err(status: LoadStatus, msg: impl Into<String>) -> Self {
        Self {
            status,
            settings: AppSettings::default(),
            parsed_plist: None,
            raw_seb_data: None,
            used_password_hash: None,
            redirect_url: None,
            message: msg.into(),
        }
    }
}

const PREFIX_PLAIN: &[u8; 4] = b"plnd";
const PREFIX_PASSWORD: &[u8; 4] = b"pswd";
const PREFIX_PASSWORD_CLIENT: &[u8; 4] = b"pwcc";
const PREFIX_PUBKEY: &[u8; 4] = b"pkhs";
const PREFIX_PUBKEY_SYMMETRIC: &[u8; 4] = b"phsk";

fn read_prefix(data: &[u8]) -> Option<[u8; 4]> {
    let head = if gzip_is_compressed(data) {
        gzip_peek(data, 4)?
    } else if data.len() >= 4 {
        data[..4].to_vec()
    } else {
        return None;
    };
    if head.len() < 4 {
        return None;
    }
    Some([head[0], head[1], head[2], head[3]])
}

pub fn is_known_binary_prefix(data: &[u8]) -> bool {
    matches!(
        read_prefix(data).as_ref(),
        Some(PREFIX_PLAIN)
            | Some(PREFIX_PASSWORD)
            | Some(PREFIX_PASSWORD_CLIENT)
            | Some(PREFIX_PUBKEY)
            | Some(PREFIX_PUBKEY_SYMMETRIC)
    )
}

pub fn load_from_bytes(
    source: &[u8],
    password: Option<&str>,
    password_is_hash: bool,
) -> LoadedConfiguration {
    load_from_bytes_inner(source, source, password, password_is_hash, 0)
}

fn load_from_bytes_inner(
    source: &[u8],
    original_raw: &[u8],
    password: Option<&str>,
    password_is_hash: bool,
    depth: usize,
) -> LoadedConfiguration {
    if source.is_empty() || depth > 4 {
        return LoadedConfiguration::err(LoadStatus::InvalidData, "Empty or nested configuration.");
    }

    // 1. JSON configuration (e.g. examples/minimal-config.json)
    let trimmed = source.iter().copied().find(|b| !b.is_ascii_whitespace());
    if trimmed == Some(b'{') {
        return parse_json_settings(source);
    }

    // 2. Uncompressed XML plist
    if is_xml_plist(source) {
        return match parse_xml_plist(source) {
            Some(tree) => build_settings_from_plist(&tree, Some(original_raw.to_vec()), None),
            None => LoadedConfiguration::err(LoadStatus::InvalidData, "Malformed XML plist."),
        };
    }

    // 3. SEB Binary container (possibly GZip-wrapped)
    let prefix = match read_prefix(source) {
        Some(p) => p,
        None => {
            return LoadedConfiguration::err(
                LoadStatus::NotSupported,
                "Unsupported SEB configuration format.",
            )
        }
    };

    if ![
        PREFIX_PLAIN,
        PREFIX_PASSWORD,
        PREFIX_PASSWORD_CLIENT,
        PREFIX_PUBKEY,
        PREFIX_PUBKEY_SYMMETRIC,
    ]
    .contains(&&prefix)
    {
        if gzip_is_compressed(source) {
            if let Some(plain) = gzip_decompress(source) {
                if is_xml_plist(&plain) {
                    if let Some(tree) = parse_xml_plist(&plain) {
                        return build_settings_from_plist(
                            &tree,
                            Some(original_raw.to_vec()),
                            None,
                        );
                    }
                }
            }
        }
        return LoadedConfiguration::err(
            LoadStatus::NotSupported,
            "Unrecognized SEB configuration header.",
        );
    }

    let mut data = if gzip_is_compressed(source) {
        match gzip_decompress(source) {
            Some(d) => d,
            None => {
                return LoadedConfiguration::err(
                    LoadStatus::InvalidData,
                    "Failed to decompress SEB configuration.",
                )
            }
        }
    } else {
        source.to_vec()
    };

    if data.len() < 4 || &data[..4] != &prefix {
        return LoadedConfiguration::err(LoadStatus::InvalidData, "Corrupt SEB container header.");
    }
    data.drain(0..4);

    if &prefix == PREFIX_PLAIN {
        return parse_plain_plist_payload(&data, original_raw, None);
    }

    if &prefix == PREFIX_PASSWORD || &prefix == PREFIX_PASSWORD_CLIENT {
        let Some(raw_pw) = password else {
            return LoadedConfiguration::err(
                LoadStatus::PasswordNeeded,
                "The SEB configuration requires a password.",
            );
        };
        let (effective_pw, used_hash) = if &prefix == PREFIX_PASSWORD_CLIENT {
            let h = if password_is_hash {
                raw_pw.to_string()
            } else {
                sha256_hex(raw_pw.as_bytes())
            };
            (h.clone(), Some(h))
        } else {
            (raw_pw.to_string(), Some(sha256_hex(raw_pw.as_bytes())))
        };
        match rncryptor_decrypt(&data, &effective_pw) {
            Ok(decrypted) => parse_plain_plist_payload(&decrypted, original_raw, used_hash),
            Err(DecryptStatus::PasswordNeeded) => LoadedConfiguration::err(
                LoadStatus::PasswordNeeded,
                "Incorrect SEB configuration password.",
            ),
            Err(_) => LoadedConfiguration::err(
                LoadStatus::InvalidData,
                "Failed to decrypt SEB configuration.",
            ),
        }
    } else {
        // Public-key RSA (`pkhs`) or hybrid symmetric (`phsk`) decryption via CertificateStore
        let store = CertificateStore::load();
        let decrypted = if &prefix == PREFIX_PUBKEY_SYMMETRIC {
            store.decrypt_phsk(&data)
        } else {
            store.decrypt_pkhs(&data)
        };
        match decrypted {
            Ok(inner) => load_from_bytes_inner(&inner, original_raw, password, password_is_hash, depth + 1),
            Err(DecryptStatus::PasswordNeeded) => LoadedConfiguration::err(
                LoadStatus::InvalidData,
                "No matching identity certificate found to decrypt this public-key .seb file.",
            ),
            Err(_) => LoadedConfiguration::err(
                LoadStatus::InvalidData,
                "Failed to decrypt public-key SEB configuration.",
            ),
        }
    }
}

fn parse_plain_plist_payload(
    payload: &[u8],
    raw_source: &[u8],
    used_hash: Option<String>,
) -> LoadedConfiguration {
    let plain = if gzip_is_compressed(payload) {
        match gzip_decompress(payload) {
            Some(d) => d,
            None => {
                return LoadedConfiguration::err(
                    LoadStatus::InvalidData,
                    "Invalid compressed plist payload.",
                )
            }
        }
    } else {
        payload.to_vec()
    };
    match parse_xml_plist(&plain) {
        Some(tree) => build_settings_from_plist(&tree, Some(raw_source.to_vec()), used_hash),
        None => LoadedConfiguration::err(LoadStatus::InvalidData, "Malformed plist inside SEB."),
    }
}

/// Fetches a local path, `file://`, `seb://`, `sebs://`, `http://`, or `https://` resource.
/// Matches C++ `NetworkResourceLoader`: blocks HTTPS->HTTP downgrades and falls back to
/// `LoadStatus::LoadWithBrowser` on HTML login redirects or network errors!
pub fn load_from_resource(
    resource: &str,
    password: Option<&str>,
    password_is_hash: bool,
) -> LoadedConfiguration {
    let trimmed = resource.trim();
    if trimmed.is_empty() {
        return LoadedConfiguration::err(LoadStatus::InvalidData, "Empty resource path.");
    }

    if let Some(path_str) = trimmed.strip_prefix("file://") {
        let decoded = percent_decode_path(path_str);
        return load_local_file(Path::new(&decoded), password, password_is_hash);
    }
    let path = Path::new(trimmed);
    if path.exists() {
        return load_local_file(path, password, password_is_hash);
    }

    let mut urls_to_try = Vec::new();
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("sebs://") {
        urls_to_try.push(format!("https://{}", &trimmed[7..]));
    } else if lower.starts_with("seb://") {
        urls_to_try.push(format!("http://{}", &trimmed[6..]));
    } else if lower.starts_with("http://") || lower.starts_with("https://") {
        urls_to_try.push(trimmed.to_string());
    } else {
        return LoadedConfiguration::err(
            LoadStatus::InvalidData,
            format!("File or resource not found: {trimmed}"),
        );
    }

    // Manual redirect loop to enforce NoLessSafeRedirectPolicy (no HTTPS -> HTTP downgrade)
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(15))
        .redirects(0)
        .user_agent(&format!(
            "SEB-Linux/{SEB_APPLICATION_VERSION} SEB/{SEB_COMPATIBILITY_VERSION}"
        ))
        .build();

    let first_url = urls_to_try[0].clone();
    for candidate in &urls_to_try {
        let mut current = candidate.clone();
        let mut hops = 0;
        loop {
            match agent.get(&current).call() {
                Ok(resp) => {
                    let status = resp.status();
                    if (300..400).contains(&status) {
                        if let Some(loc) = resp.header("location") {
                            hops += 1;
                            if hops > 8 {
                                break;
                            }
                            if let Ok(base) = Url::parse(&current) {
                                if let Ok(next) = base.join(loc) {
                                    // Block HTTPS -> HTTP downgrade
                                    if base.scheme() == "https" && next.scheme() == "http" {
                                        let mut res = LoadedConfiguration::err(
                                            LoadStatus::LoadWithBrowser,
                                            "Insecure redirect blocked; opening in browser.",
                                        );
                                        res.redirect_url = Some(first_url);
                                        return res;
                                    }
                                    current = next.to_string();
                                    continue;
                                }
                            }
                        }
                    }

                    let _final_url = resp.get_url().to_string();
                    let content_type = resp
                        .header("content-type")
                        .unwrap_or("")
                        .to_ascii_lowercase();
                    let mut bytes = Vec::new();
                    if resp
                        .into_reader()
                        .take(32 * 1024 * 1024)
                        .read_to_end(&mut bytes)
                        .is_err()
                    {
                        break;
                    }

                    let looks_html = content_type.contains("text/html")
                        || bytes
                            .windows(5)
                            .take(128)
                            .any(|w| w.eq_ignore_ascii_case(b"<html") || w.eq_ignore_ascii_case(b"<!doc"));
                    if looks_html && !is_xml_plist(&bytes) && !is_known_binary_prefix(&bytes) {
                        let mut res = LoadedConfiguration::err(
                            LoadStatus::LoadWithBrowser,
                            "Configuration requires browser authentication.",
                        );
                        res.redirect_url = Some(first_url);
                        return res;
                    }

                    let mut loaded = load_from_bytes(&bytes, password, password_is_hash);
                    if loaded.status == LoadStatus::Success {
                        apply_start_url_query_from_resource(candidate, &mut loaded.settings);
                    }
                    return loaded;
                }
                Err(ureq::Error::Status(code, resp)) => {
                    if matches!(code, 301 | 302 | 303 | 307 | 308) {
                        if let Some(loc) = resp.header("location") {
                            hops += 1;
                            if hops <= 8 {
                                if let Ok(base) = Url::parse(&current) {
                                    if let Ok(next) = base.join(loc) {
                                        if base.scheme() == "https" && next.scheme() == "http" {
                                            let mut res = LoadedConfiguration::err(
                                                LoadStatus::LoadWithBrowser,
                                                "Insecure redirect blocked; opening in browser.",
                                            );
                                            res.redirect_url = Some(first_url);
                                            return res;
                                        }
                                        current = next.to_string();
                                        continue;
                                    }
                                }
                            }
                        }
                    }
                    let mut res = LoadedConfiguration::err(
                        LoadStatus::LoadWithBrowser,
                        format!("Remote configuration returned HTTP {code}; opening in browser."),
                    );
                    res.redirect_url = Some(first_url);
                    return res;
                }
                Err(_) => break,
            }
        }
    }

    // Matches C++ NetworkResourceLoader: network failure falls back to LoadWithBrowser
    let mut res = LoadedConfiguration::err(
        LoadStatus::LoadWithBrowser,
        "Opening remote configuration resource in browser.",
    );
    res.redirect_url = Some(first_url);
    res
}

pub fn apply_start_url_query_from_resource(resource: &str, settings: &mut AppSettings) {
    if let Ok(parsed_url) = Url::parse(resource) {
        if let Some(query) = parsed_url.query() {
            if let Some(sep) = query.rfind('?') {
                if sep > 0 {
                    settings.browser.start_url_query = query[sep..].to_string();
                }
            }
        }
    }
}

fn load_local_file(
    path: &Path,
    password: Option<&str>,
    password_is_hash: bool,
) -> LoadedConfiguration {
    match fs::read(path) {
        Ok(bytes) => load_from_bytes(&bytes, password, password_is_hash),
        Err(e) => LoadedConfiguration::err(
            LoadStatus::UnexpectedError,
            format!("Failed to read {}: {e}", path.display()),
        ),
    }
}

fn percent_decode_path(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(hex_str) = std::str::from_utf8(&bytes[i + 1..i + 3]) {
                if let Ok(val) = u8::from_str_radix(hex_str, 16) {
                    out.push(val);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 100% parity with C++ `ConfigurationRepository::configureClientWith`:
/// - Imports & strips type-1 PKCS#12 identity certs into `CertificateStore` (`libsecret`).
/// - If `keep_encryption == false`, re-encrypts plist with default `pwcc` empty-password hash.
/// - If `keep_encryption == true`, preserves original encrypted bytes.
pub fn save_client_configuration(loaded: &LoadedConfiguration, dest: &Path) -> Result<(), String> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Could not create config directory: {e}"))?;
    }

    let payload = if loaded.settings.keep_client_config_encryption {
        if let Some(ref raw) = loaded.raw_seb_data {
            raw.clone()
        } else if let Some(ref plist) = loaded.parsed_plist {
            serialize_binary_plain(plist)
                .ok_or_else(|| "Failed serializing client configuration.".to_string())?
        } else {
            return Err("No configuration payload available to save.".to_string());
        }
    } else if let Some(mut plist) = loaded.parsed_plist.clone() {
        let mut store = CertificateStore::load();
        store.extract_and_import_identities(&mut plist);
        // Default client config encryption: pwcc with empty password hash (matches C++)
        let empty_hash = sha256_hex(b"");
        serialize_binary_password(&plist, &empty_hash, true)
            .ok_or_else(|| "Failed encrypting client configuration.".to_string())?
    } else if let Some(ref raw) = loaded.raw_seb_data {
        raw.clone()
    } else {
        return Err("No configuration payload available to save.".to_string());
    };

    fs::write(dest, payload).map_err(|e| format!("Could not save client configuration: {e}"))
}

/// 100% parity with C++ `AppController` `LoadWithBrowser` clean login profile:
/// Applied when a remote `.seb`/`sebs://` URL redirects to an LMS login page (e.g. Moodle SSO).
pub fn apply_browser_resource_profile(cfg: &mut SessionConfiguration, login_url: &str) {
    cfg.is_browser_resource = true;
    cfg.is_client_configuration = false;
    let s = &mut cfg.settings;
    s.applications.blacklist.clear();
    s.applications.whitelist.clear();
    s.browser.start_url = login_url.to_string();
    s.browser.allow_configuration_downloads = true;
    s.browser.delete_cache_on_shutdown = false;
    s.browser.delete_cookies_on_shutdown = false;
    s.browser.delete_cookies_on_startup = false;
    s.browser.main_window.show_toolbar = false;
    s.browser.main_window.full_screen_mode = true;
    s.browser.main_window.show_reload_button = false;
    s.browser.main_window.show_reload_warning = false;
    s.browser.main_window.allow_backward_navigation = true;
    s.browser.main_window.allow_forward_navigation = true;
    s.browser.main_window.allow_reloading = true;
    s.browser.main_window.url_policy = UrlPolicy::Never;
    s.display.allowed_displays = 10;
    s.display.ignore_error = true;
    s.display.internal_display_only = false;
    s.security.allow_reconfiguration = true;
    s.security.verify_session_integrity = false;
    s.security.verify_cursor_configuration = false;
    s.security.virtual_machine_policy = VirtualMachinePolicy::Allow;
    s.security.kiosk_mode = KioskMode::None;
    s.security.allow_window_capture = true;
    s.service.ignore_service = true;
    s.user_interface.action_center.enable_action_center = true;
    s.user_interface.action_center.show_network = true;
    s.user_interface.taskbar.show_network = true;
    s.proctoring.enabled = false;
    s.proctoring.screen_proctoring.enabled = false;
    apply_start_url_query_from_resource(login_url, s);
}

/// Validates Custom ProxySettings matching C++ `webengine_environment.cpp`.
pub fn validate_proxy_settings(proxy: &ProxySettings) -> Result<(), String> {
    if proxy.policy != ProxyPolicy::Custom {
        return Ok(());
    }
    if proxy.auto_configure {
        let u = Url::parse(proxy.auto_configure_url.trim())
            .map_err(|_| "Invalid proxy auto-configuration URL.".to_string())?;
        if !matches!(u.scheme(), "http" | "https")
            || !u.username().is_empty()
            || u.password().is_some()
            || u.fragment().is_some()
        {
            return Err("Proxy auto-configuration URL must be clean http/https.".to_string());
        }
    }
    let mut seen = Vec::new();
    for p in &proxy.proxies {
        if seen.contains(&p.protocol) {
            return Err("Duplicate proxy configuration for the same protocol.".to_string());
        }
        seen.push(p.protocol);
        let h = p.host.trim();
        if h.is_empty()
            || h.chars()
                .any(|c| c.is_whitespace() || matches!(c, '/' | '@' | ';' | '?' | '#' | '='))
        {
            return Err(format!("Invalid proxy host: {h}"));
        }
        if p.port == 0 {
            return Err("Proxy port must be between 1 and 65535.".to_string());
        }
    }
    Ok(())
}

// =========================================================================
// DataProcessor (100% parity with C++ DataProcessor)
// =========================================================================

pub fn process_settings_default(settings: &mut AppSettings) {
    // CRITICAL (AGENTS.md): Keep allowBrowserToolbarForReloading active so Moodle .seb
    // configs (showReloadButton=true + browserWindowAllowReload=true) reveal top bar!
    if settings.browser.additional_window.allow_reloading
        && settings.browser.additional_window.show_reload_button
    {
        settings.browser.additional_window.show_toolbar = true;
    }
    if settings.browser.main_window.allow_reloading
        && settings.browser.main_window.show_reload_button
    {
        settings.browser.main_window.show_toolbar = true;
    }

    settings.browser.main_window.show_home_button = settings.browser.use_start_url_as_home_url
        || !settings.browser.home_url.trim().is_empty();
    settings.browser.home_password_hash = settings.security.quit_password_hash.clone();

    settings.browser.use_isolated_clipboard =
        settings.security.clipboard_policy == ClipboardPolicy::Isolated;
    let clipboard_allowed = settings.security.clipboard_policy != ClipboardPolicy::Block;
    settings.keyboard.allow_ctrl_c = clipboard_allowed;
    settings.keyboard.allow_ctrl_v = clipboard_allowed;
    settings.keyboard.allow_ctrl_x = clipboard_allowed;

    settings.proctoring.enabled = settings.proctoring.screen_proctoring.enabled;

    settings.applications.whitelist.retain(|app| {
        let is_xul = app.executable_path.contains("xulrunner");
        let is_ff = app.executable_name.eq_ignore_ascii_case("firefox.exe")
            || app.executable_name.eq_ignore_ascii_case("xulrunner.exe");
        !(is_xul && is_ff)
    });
}

fn build_settings_from_plist(
    root: &SebValue,
    raw_seb: Option<Vec<u8>>,
    used_hash: Option<String>,
) -> LoadedConfiguration {
    if !validate_plist_data(root) {
        return LoadedConfiguration::err(
            LoadStatus::InvalidData,
            "SEB configuration failed data validation (invalid password hash or malformed string).",
        );
    }

    let mut settings = AppSettings::default();
    if let SebValue::Map(entries) = root {
        map_plist_entries(entries, &mut settings);
    } else {
        return LoadedConfiguration::err(
            LoadStatus::InvalidData,
            "Root of SEB plist must be a dictionary.",
        );
    }

    process_settings_default(&mut settings);

    let canonical = serialize_canonical_json(root);
    settings.browser.configuration_key = sha256_hex(&canonical);

    LoadedConfiguration {
        status: LoadStatus::Success,
        settings,
        parsed_plist: Some(root.clone()),
        raw_seb_data: raw_seb,
        used_password_hash: used_hash,
        redirect_url: None,
        message: String::new(),
    }
}

// =========================================================================
// Plist Key Helpers & Complete SEB Data Mapper (100% C++ Parity)
// =========================================================================

fn find_val<'a>(entries: &'a [(String, SebValue)], key: &str) -> Option<&'a SebValue> {
    entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

fn get_bool(entries: &[(String, SebValue)], key: &str) -> Option<bool> {
    match find_val(entries, key)? {
        SebValue::Bool(b) => Some(*b),
        SebValue::Int(i) => Some(*i != 0),
        _ => None,
    }
}

fn get_i64(entries: &[(String, SebValue)], key: &str) -> Option<i64> {
    match find_val(entries, key)? {
        SebValue::Int(i) => Some(*i),
        SebValue::Uint(u) => Some(*u as i64),
        SebValue::Float(f) => Some(*f as i64),
        SebValue::String(s) => s.trim().parse::<i64>().ok(),
        _ => None,
    }
}

fn get_f64(entries: &[(String, SebValue)], key: &str) -> Option<f64> {
    match find_val(entries, key)? {
        SebValue::Float(f) => Some(*f),
        SebValue::Int(i) => Some(*i as f64),
        SebValue::Uint(u) => Some(*u as f64),
        SebValue::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

fn get_str(entries: &[(String, SebValue)], key: &str) -> Option<String> {
    match find_val(entries, key)? {
        SebValue::String(s) => Some(s.clone()),
        SebValue::Int(i) => Some(i.to_string()),
        _ => None,
    }
}

fn get_bytes(entries: &[(String, SebValue)], key: &str) -> Option<Vec<u8>> {
    match find_val(entries, key)? {
        SebValue::Bytes(b64) => base64::engine::general_purpose::STANDARD.decode(b64).ok(),
        _ => None,
    }
}

fn map_url_policy(v: i64) -> UrlPolicy {
    match v {
        0 => UrlPolicy::Never,
        1 => UrlPolicy::LoadError,
        2 => UrlPolicy::BeforeTitle,
        3 => UrlPolicy::Always,
        _ => UrlPolicy::Never,
    }
}

fn map_window_position(v: i64) -> WindowPosition {
    match v {
        0 => WindowPosition::Left,
        1 => WindowPosition::Center,
        2 => WindowPosition::Right,
        _ => WindowPosition::Left,
    }
}

fn parse_window_dimension(val: Option<&SebValue>) -> (Option<i32>, Option<i32>) {
    let Some(v) = val else {
        return (None, None);
    };
    match v {
        SebValue::String(s) => {
            let trimmed = s.trim();
            if let Some(pct) = trimmed.strip_suffix('%') {
                (pct.trim().parse::<i32>().ok(), None)
            } else {
                (None, trimmed.parse::<i32>().ok())
            }
        }
        SebValue::Int(i) => (None, Some(*i as i32)),
        _ => (None, None),
    }
}

fn map_plist_entries(e: &[(String, SebValue)], s: &mut AppSettings) {
    // Configuration File
    if let Some(v) = get_i64(e, "sebConfigPurpose") {
        s.configuration_mode = if v == 1 {
            ConfigurationMode::ConfigureClient
        } else {
            ConfigurationMode::Exam
        };
    }
    if let Some(v) = get_bool(e, "clientConfigKeepEncryption") {
        s.keep_client_config_encryption = v;
    }
    if let Some(v) = get_i64(e, "sebMode") {
        s.session_mode = if v == 1 {
            SessionMode::Server
        } else {
            SessionMode::Normal
        };
    }

    // General & System
    if let Some(v) = get_i64(e, "logLevel") {
        s.log_level = match v {
            0 => LogLevel::Error,
            1 => LogLevel::Warning,
            2 => LogLevel::Info,
            _ => LogLevel::Debug,
        };
    }
    if let Some(v) = get_bool(e, "systemAlwaysOn") {
        s.system_always_on = v;
    }

    // UserInterface mode FIRST (needed by Desktop vs Mobile UA selection!)
    if let Some(v) = get_bool(e, "touchOptimized") {
        s.user_interface.mode = if v {
            UserInterfaceMode::Mobile
        } else {
            UserInterfaceMode::Desktop
        };
    }

    // Browser
    if let Some(v) = get_bool(e, "enableSebBrowser") {
        s.browser.enable_browser = v;
    }
    if let Some(v) = get_str(e, "startURL") {
        if !v.trim().is_empty() {
            s.browser.start_url = v;
        }
    }
    if let Some(v) = get_bool(e, "startURLAppendQueryParameter") {
        s.browser.use_query_parameter = v;
    }
    if let Some(v) = get_str(e, "quitURL") {
        s.browser.quit_url = v;
    }
    if let Some(v) = get_bool(e, "quitURLConfirm") {
        s.browser.confirm_quit_url = v;
    }
    if let Some(v) = get_bool(e, "quitURLRestart") {
        s.browser.reset_on_quit_url = v;
    }
    if let Some(v) = get_str(e, "restartExamURL") {
        s.browser.home_url = v;
    }
    if let Some(v) = get_bool(e, "restartExamUseStartURL") {
        s.browser.use_start_url_as_home_url = v;
    }
    if let Some(v) = get_str(e, "restartExamText") {
        s.browser.home_navigation_message = v;
    }
    if let Some(v) = get_bool(e, "restartExamPasswordProtected") {
        s.browser.home_navigation_requires_password = v;
    }
    if let Some(v) = get_bool(e, "sendBrowserExamKey") {
        s.browser.send_browser_exam_key = v;
        s.browser.send_configuration_key = v;
    }
    if let Some(salt) = get_bytes(e, "examKeySalt") {
        s.browser.exam_key_salt = salt;
    }
    if let Some(v) = get_bool(e, "downloadAndOpenSebConfig") {
        s.browser.allow_configuration_downloads = v;
    }
    if let Some(v) = get_bool(e, "browserMediaCaptureCamera") {
        s.browser.media_capture_camera = v;
    }
    if let Some(v) = get_bool(e, "browserMediaCaptureMicrophone") {
        s.browser.media_capture_microphone = v;
    }
    if let Some(v) = get_bool(e, "allowDownloads") {
        s.browser.allow_downloads = v;
    }
    if let Some(v) = get_bool(e, "allowUploads") {
        s.browser.allow_uploads = v;
    }
    if let Some(v) = get_bool(e, "allowCustomDownUploadLocation") {
        s.browser.allow_custom_down_upload_location = v;
    }
    if let Some(v) = get_bool(e, "useTemporaryDownUploadDirectory") {
        s.browser.use_temporary_down_upload_directory = v;
    }
    if let Some(v) = get_str(e, "downloadDirectoryWin") {
        s.browser.down_upload_directory = v;
    }
    if let Some(v) = get_bool(e, "browserShowFileSystemElementPath") {
        s.browser.show_file_system_element_path = v;
    }
    if let Some(v) = get_bool(e, "allowFind") {
        s.browser.allow_find = v;
    }
    if let Some(v) = get_bool(e, "enableZoomPage") {
        s.browser.allow_page_zoom = v;
    }
    // Fixed parity: downloadPDFFiles -> !allow_pdf_reader, allowPDFReaderToolbar -> separate field
    if let Some(v) = get_bool(e, "downloadPDFFiles") {
        s.browser.allow_pdf_reader = !v;
    }
    if let Some(v) = get_bool(e, "allowPDFReaderToolbar") {
        s.browser.allow_pdf_reader_toolbar = v;
    }
    if let Some(v) = get_bool(e, "allowPrint") {
        s.browser.allow_print = v;
    }
    if let Some(v) = get_bool(e, "allowSpellCheck") {
        s.browser.allow_spell_checking = v;
    }
    if let Some(v) = get_bool(e, "allowDeveloperConsole") {
        s.browser.main_window.allow_developer_console = v;
        s.browser.additional_window.allow_developer_console = v;
    }
    if let Some(v) = get_bool(e, "removeBrowserProfile") {
        s.browser.delete_cache_on_shutdown = v;
    }
    if let Some(v) = get_bool(e, "examSessionClearCookiesOnStart") {
        s.browser.delete_cookies_on_startup = v;
    }
    if let Some(v) = get_bool(e, "examSessionClearCookiesOnEnd") {
        s.browser.delete_cookies_on_shutdown = v;
    }
    if let Some(v) = get_str(e, "browserUserAgent") {
        s.browser.user_agent_suffix = v;
    }
    // Desktop vs Mobile User-Agent selection (matches browser_data_mapper.cpp)
    if s.user_interface.mode == UserInterfaceMode::Mobile {
        if let Some(v) = get_i64(e, "browserUserAgentWinTouchMode") {
            s.browser.use_custom_user_agent = v == 2;
            s.browser.user_agent_mode = if v == 2 {
                UserAgentMode::Custom
            } else {
                UserAgentMode::Default
            };
        }
        if let Some(v) = get_str(e, "browserUserAgentWinTouchModeCustom") {
            if !v.trim().is_empty() {
                s.browser.custom_user_agent = v;
            }
        }
    } else {
        if let Some(v) = get_i64(e, "browserUserAgentWinDesktopMode") {
            s.browser.use_custom_user_agent = v == 1;
            s.browser.user_agent_mode = if v == 1 {
                UserAgentMode::Custom
            } else {
                UserAgentMode::Default
            };
        }
        if let Some(v) = get_str(e, "browserUserAgentWinDesktopModeCustom") {
            if !v.trim().is_empty() {
                s.browser.custom_user_agent = v;
            }
        }
    }

    if let Some(v) = get_bool(e, "enableBrowserWindowToolbar") {
        s.browser.main_window.show_toolbar = v;
        s.browser.additional_window.show_toolbar = v;
    }
    if let Some(v) = get_bool(e, "showReloadButton") {
        s.browser.main_window.show_reload_button = v;
        s.browser.additional_window.show_reload_button = v;
    }
    if let Some(v) = get_bool(e, "browserWindowAllowAddressBar") {
        s.browser.main_window.allow_address_bar = v;
    }
    if let Some(v) = get_bool(e, "allowBrowsingBackForward") {
        s.browser.main_window.allow_backward_navigation = v;
        s.browser.main_window.allow_forward_navigation = v;
    }
    if let Some(v) = get_bool(e, "browserWindowAllowReload") {
        s.browser.main_window.allow_reloading = v;
    }
    if let Some(v) = get_bool(e, "showReloadWarning") {
        s.browser.main_window.show_reload_warning = v;
    }
    if let Some(v) = get_i64(e, "browserWindowShowURL") {
        s.browser.main_window.url_policy = map_url_policy(v);
    }
    if let Some(v) = get_i64(e, "browserViewMode") {
        s.browser.main_window.full_screen_mode = v == 1;
    }
    if let Some(v) = get_i64(e, "mainBrowserWindowPositioning") {
        s.browser.main_window.position = map_window_position(v);
    }
    let (rw, aw) = parse_window_dimension(find_val(e, "mainBrowserWindowWidth"));
    if rw.is_some() || aw.is_some() {
        s.browser.main_window.relative_width = rw;
        s.browser.main_window.absolute_width = aw;
    }
    let (rh, ah) = parse_window_dimension(find_val(e, "mainBrowserWindowHeight"));
    if rh.is_some() || ah.is_some() {
        s.browser.main_window.relative_height = rh;
        s.browser.main_window.absolute_height = ah;
    }

    // Additional window
    if let Some(v) = get_bool(e, "newBrowserWindowAllowAddressBar") {
        s.browser.additional_window.allow_address_bar = v;
    }
    if let Some(v) = get_bool(e, "newBrowserWindowNavigation") {
        s.browser.additional_window.allow_backward_navigation = v;
        s.browser.additional_window.allow_forward_navigation = v;
    }
    if let Some(v) = get_bool(e, "newBrowserWindowAllowReload") {
        s.browser.additional_window.allow_reloading = v;
    }
    if let Some(v) = get_bool(e, "newBrowserWindowShowReloadWarning") {
        s.browser.additional_window.show_reload_warning = v;
    }
    if let Some(v) = get_i64(e, "newBrowserWindowShowURL") {
        s.browser.additional_window.url_policy = map_url_policy(v);
    }
    if let Some(v) = get_i64(e, "newBrowserWindowByLinkPositioning") {
        s.browser.additional_window.position = map_window_position(v);
    }
    let (rw, aw) = parse_window_dimension(find_val(e, "newBrowserWindowByLinkWidth"));
    if rw.is_some() || aw.is_some() {
        s.browser.additional_window.relative_width = rw;
        s.browser.additional_window.absolute_width = aw;
    }
    let (rh, ah) = parse_window_dimension(find_val(e, "newBrowserWindowByLinkHeight"));
    if rh.is_some() || ah.is_some() {
        s.browser.additional_window.relative_height = rh;
        s.browser.additional_window.absolute_height = ah;
    }

    // Fixed PopupPolicy mapping (100% parity with browser_data_mapper.cpp:124-133!)
    if let Some(policy) = get_i64(e, "newBrowserWindowByLinkPolicy") {
        let block_foreign = get_bool(e, "newBrowserWindowByLinkBlockForeign").unwrap_or(false);
        s.browser.popup_policy = match (policy, block_foreign) {
            (0, _) => PopupPolicy::Block,
            (1, true) => PopupPolicy::AllowSameHostAndWindow,
            (1, false) => PopupPolicy::AllowSameWindow,
            (2, true) => PopupPolicy::AllowSameHost,
            (2, false) => PopupPolicy::Allow,
            _ => PopupPolicy::Allow,
        };
    }

    // Proxy settings (100% parity with browser_data_mapper.cpp:97-116)
    if let Some(v) = get_i64(e, "proxySettingsPolicy") {
        s.browser.proxy.policy = if v == 1 {
            ProxyPolicy::Custom
        } else {
            ProxyPolicy::System
        };
    }
    if let Some(SebValue::Map(pm)) = find_val(e, "proxies") {
        if let Some(v) = get_bool(pm, "AutoConfigurationEnabled") {
            s.browser.proxy.auto_configure = v;
        }
        if let Some(v) = get_str(pm, "AutoConfigurationURL") {
            s.browser.proxy.auto_configure_url = v;
        }
        if let Some(v) = get_bool(pm, "AutoDiscoveryEnabled") {
            s.browser.proxy.auto_detect = v;
        }
        if let Some(SebValue::List(list)) = find_val(pm, "ExceptionsList") {
            s.browser.proxy.bypass_list = list
                .iter()
                .filter_map(|item| match item {
                    SebValue::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
                    _ => None,
                })
                .collect();
        }
        let specs = [
            (
                ProxyProtocol::Ftp,
                "FTPEnable",
                "FTPProxy",
                "FTPPort",
                "FTPRequiresPassword",
                "FTPUsername",
                "FTPPassword",
            ),
            (
                ProxyProtocol::Http,
                "HTTPEnable",
                "HTTPProxy",
                "HTTPPort",
                "HTTPRequiresPassword",
                "HTTPUsername",
                "HTTPPassword",
            ),
            (
                ProxyProtocol::Https,
                "HTTPSEnable",
                "HTTPSProxy",
                "HTTPSPort",
                "HTTPSRequiresPassword",
                "HTTPSUsername",
                "HTTPSPassword",
            ),
            (
                ProxyProtocol::Socks,
                "SOCKSEnable",
                "SOCKSProxy",
                "SOCKSPort",
                "SOCKSRequiresPassword",
                "SOCKSUsername",
                "SOCKSPassword",
            ),
        ];
        s.browser.proxy.proxies.clear();
        for (proto, en, host_k, port_k, req_k, user_k, pass_k) in specs {
            if get_bool(pm, en).unwrap_or(false) {
                s.browser.proxy.proxies.push(ProxyConfiguration {
                    protocol: proto,
                    host: get_str(pm, host_k).unwrap_or_default(),
                    port: get_i64(pm, port_k).unwrap_or(0).clamp(0, 65535) as u16,
                    requires_authentication: get_bool(pm, req_k).unwrap_or(false),
                    username: get_str(pm, user_k).unwrap_or_default(),
                    password: get_str(pm, pass_k).unwrap_or_default(),
                });
            }
        }
    }

    // URL Filter (fixed: ProcessContentRequests = processMain && processContent)
    if let Some(v) = get_bool(e, "URLFilterEnable") {
        s.browser.filter.process_main_requests = v;
    }
    if let Some(v) = get_bool(e, "URLFilterEnableContentFilter") {
        s.browser.filter.process_content_requests = s.browser.filter.process_main_requests && v;
    }
    if let Some(SebValue::List(items)) = find_val(e, "URLFilterRules") {
        s.browser.filter.rules.clear();
        for item in items {
            if let SebValue::Map(rule_map) = item {
                if !get_bool(rule_map, "active").unwrap_or(true) {
                    continue;
                }
                let expr = get_str(rule_map, "expression").unwrap_or_default();
                if expr.trim().is_empty() {
                    continue;
                }
                let allow = get_i64(rule_map, "action").unwrap_or(0) == 1;
                let is_regex = get_bool(rule_map, "regex").unwrap_or(false);
                s.browser.filter.rules.push(FilterRuleConfig {
                    active: true,
                    expression: expr,
                    is_regex,
                    allow,
                });
            }
        }
    }

    // Security (100% parity with security_data_mapper.cpp)
    if let Some(v) = get_str(e, "hashedAdminPassword") {
        s.security.admin_password_hash = v;
    }
    if let Some(v) = get_str(e, "hashedQuitPassword") {
        s.security.quit_password_hash = v;
    }
    if let Some(v) = get_bool(e, "allowQuit") {
        s.security.allow_termination = v;
    }
    if let Some(v) = get_bool(e, "allowApplicationLog") {
        s.security.allow_application_log = v;
    }
    if let Some(v) = get_bool(e, "examSessionReconfigureAllow") {
        s.security.allow_reconfiguration = v;
    }
    if let Some(v) = get_str(e, "examSessionReconfigureConfigURL") {
        s.security.reconfiguration_url = v;
    }
    if let Some(v) = get_bool(e, "allowStickyKeys") {
        s.security.allow_sticky_keys = v;
    }
    if let Some(v) = get_bool(e, "disableSessionChangeLockScreen") {
        s.security.disable_session_change_lock_screen = v;
    }
    if let Some(v) = get_bool(e, "enableCursorVerification") {
        s.security.verify_cursor_configuration = v;
    }
    if let Some(v) = get_bool(e, "allowVirtualMachine") {
        s.security.virtual_machine_policy = if v {
            VirtualMachinePolicy::Allow
        } else {
            VirtualMachinePolicy::Deny
        };
    }
    if let Some(v) = get_bool(e, "allowScreenSharing") {
        s.security.allow_window_capture = v;
    }
    if let Some(v) = get_bool(e, "enableSessionVerification") {
        s.security.verify_session_integrity = v;
    }
    if let Some(v) = get_i64(e, "clipboardPolicy") {
        s.security.clipboard_policy = match v {
            0 => ClipboardPolicy::Allow,
            1 => ClipboardPolicy::Block,
            _ => ClipboardPolicy::Isolated,
        };
    }
    // Check createNewDesktop and killExplorerShell independently
    let create_new_desktop = get_bool(e, "createNewDesktop");
    let kill_explorer = get_bool(e, "killExplorerShell");
    if create_new_desktop == Some(true) {
        s.security.kiosk_mode = KioskMode::CreateNewDesktop;
    } else if kill_explorer == Some(true) {
        s.security.kiosk_mode = KioskMode::DisableExplorerShell;
    } else if create_new_desktop == Some(false) || kill_explorer == Some(false) {
        s.security.kiosk_mode = KioskMode::None;
    }
    // Fixed sebAllowedVersions: supports BOTH plist <array> of "win.3.7.0.min" AND string fallback!
    if let Some(val) = find_val(e, "sebAllowedVersions") {
        s.security.version_restrictions = parse_version_restrictions_value(val);
    }

    // Service settings (all 16 keys, 100% parity with service_data_mapper.cpp)
    if let Some(v) = get_bool(e, "sebServiceIgnore") {
        s.service.ignore_service = v;
    }
    if let Some(v) = get_i64(e, "sebServicePolicy") {
        s.service.policy = match v {
            1 => ServicePolicy::Warn,
            2 => ServicePolicy::Mandatory,
            _ => ServicePolicy::Optional,
        };
    }
    if let Some(v) = get_bool(e, "enableChromeNotifications") {
        s.service.disable_chrome_notifications = !v;
    }
    if let Some(v) = get_bool(e, "insideSebEnableEaseOfAccess") {
        s.service.disable_ease_of_access_options = !v;
    }
    if let Some(v) = get_bool(e, "enableFindPrinter") {
        s.service.disable_find_printer = !v;
    }
    if let Some(v) = get_bool(e, "insideSebEnableNetworkConnectionSelector") {
        s.service.disable_network_options = !v;
    }
    if let Some(v) = get_bool(e, "insideSebEnableChangeAPassword") {
        s.service.disable_password_change = !v;
    }
    if let Some(v) = get_bool(e, "insideSebEnableShutDown") {
        s.service.disable_power_options = !v;
    }
    if let Some(v) = get_bool(e, "allowScreenSharing") {
        s.service.disable_remote_connections = !v;
    }
    if let Some(v) = get_bool(e, "insideSebEnableLogOff") {
        s.service.disable_signout = !v;
    }
    if let Some(v) = get_bool(e, "insideSebEnableStartTaskManager") {
        s.service.disable_task_manager = !v;
    }
    if let Some(v) = get_bool(e, "insideSebEnableLockThisComputer") {
        s.service.disable_user_lock = !v;
    }
    if let Some(v) = get_bool(e, "insideSebEnableSwitchUser") {
        s.service.disable_user_switch = !v;
    }
    if let Some(v) = get_bool(e, "insideSebEnableVmWareClientShade") {
        s.service.disable_vmware_overlay = !v;
    }
    if let Some(v) = get_bool(e, "enableWindowsUpdate") {
        s.service.disable_windows_update = !v;
    }
    if let Some(v) = get_bool(e, "setVmwareConfiguration") {
        s.service.set_vmware_configuration = v;
    }

    // Proctoring (Screen Proctoring — fixed string image format mapping!)
    if let Some(v) = get_bool(e, "showProctoringDisclaimer") {
        s.proctoring.show_disclaimer = v;
    }
    if let Some(v) = get_bool(e, "showProctoringViewButton") {
        s.proctoring.show_taskbar_notification = v;
    }
    if let Some(v) = get_bool(e, "enableScreenProctoring") {
        s.proctoring.screen_proctoring.enabled = v;
    }
    if let Some(v) = get_str(e, "screenProctoringServiceURL") {
        s.proctoring.screen_proctoring.service_url = v;
    }
    if let Some(v) = get_str(e, "screenProctoringClientId") {
        s.proctoring.screen_proctoring.client_id = v;
    }
    if let Some(v) = get_str(e, "screenProctoringClientSecret") {
        s.proctoring.screen_proctoring.client_secret = v;
    }
    if let Some(v) = get_str(e, "screenProctoringGroupId") {
        s.proctoring.screen_proctoring.group_id = v;
    }
    if let Some(v) = get_str(e, "screenProctoringEncryptSecret") {
        s.proctoring.screen_proctoring.encryption_secret = v;
    }
    if let Some(v) = get_i64(e, "screenProctoringCacheSize") {
        if v > 0 {
            s.proctoring.screen_proctoring.cache_size = v as usize;
        }
    }
    if let Some(v) = get_f64(e, "screenProctoringImageDownscale") {
        s.proctoring.screen_proctoring.image_downscaling = v;
    }
    if let Some(v) = get_i64(e, "screenProctoringScreenshotMinInterval") {
        if v > 0 {
            s.proctoring.screen_proctoring.interval_minimum = v as u64;
        }
    }
    if let Some(v) = get_i64(e, "screenProctoringScreenshotMaxInterval") {
        if v > 0 {
            s.proctoring.screen_proctoring.interval_maximum = v as u64;
        }
    }
    if let Some(val) = find_val(e, "screenProctoringImageFormat") {
        match val {
            SebValue::String(fmt) => {
                s.proctoring.screen_proctoring.image_format = match fmt.trim().to_ascii_lowercase().as_str() {
                    "bmp" => ProctoringImageFormat::Bmp,
                    "gif" => ProctoringImageFormat::Gif,
                    "jpg" | "jpeg" => ProctoringImageFormat::Jpg,
                    "png" => ProctoringImageFormat::Png,
                    _ => ProctoringImageFormat::Png,
                };
            }
            SebValue::Int(i) => {
                s.proctoring.screen_proctoring.image_format = match *i {
                    0 => ProctoringImageFormat::Png,
                    1 => ProctoringImageFormat::Jpg,
                    2 => ProctoringImageFormat::Bmp,
                    3 => ProctoringImageFormat::Gif,
                    _ => ProctoringImageFormat::Png,
                };
            }
            _ => {}
        }
    }
    if let Some(v) = get_i64(e, "screenProctoringImageQuantization") {
        s.proctoring.screen_proctoring.image_quantization = match v {
            0 => ProctoringImageQuantization::BlackAndWhite1bpp,
            1 => ProctoringImageQuantization::Grayscale2bpp,
            2 => ProctoringImageQuantization::Grayscale4bpp,
            3 => ProctoringImageQuantization::Grayscale8bpp,
            4 => ProctoringImageQuantization::Color8bpp,
            5 => ProctoringImageQuantization::Color16bpp,
            _ => ProctoringImageQuantization::Color24bpp,
        };
    }
    if let Some(v) = get_bool(e, "screenProctoringMetadataActiveAppEnabled") {
        s.proctoring.screen_proctoring.meta_data.capture_application_data = v;
    }
    if let Some(v) = get_bool(e, "screenProctoringMetadataURLEnabled") {
        s.proctoring.screen_proctoring.meta_data.capture_browser_data = v;
    }
    if let Some(v) = get_bool(e, "screenProctoringMetadataWindowTitleEnabled") {
        s.proctoring.screen_proctoring.meta_data.capture_window_title = v;
    }

    // SEB Server
    if let Some(v) = get_str(e, "sebServerURL") {
        s.server.server_url = v;
    }
    if let Some(v) = get_bool(e, "sebServerFallback") {
        s.server.perform_fallback = v;
    }
    if let Some(v) = get_str(e, "sebServerFallbackPasswordHash") {
        s.server.fallback_password_hash = v;
    }
    if let Some(v) = get_i64(e, "sebServerFallbackAttempts") {
        if v > 0 {
            s.server.request_attempts = v as u32;
        }
    }
    if let Some(v) = get_i64(e, "sebServerFallbackAttemptInterval") {
        if v > 0 {
            s.server.request_attempt_interval_ms = v as u64;
        }
    }
    if let Some(v) = get_i64(e, "sebServerFallbackTimeout") {
        if v > 0 {
            s.server.request_timeout_ms = v as u64;
        }
    }
    if let Some(v) = get_bool(e, "raiseHandButtonShow") {
        s.server.show_raise_hand = v;
    }
    if let Some(v) = get_bool(e, "raiseHandButtonAlwaysPromptMessage") {
        s.server.force_raise_hand_message = v;
    }
    if let Some(SebValue::Map(srv)) = find_val(e, "sebServerConfiguration") {
        if let Some(v) = get_str(srv, "apiDiscovery") {
            s.server.api_url = v;
        }
        if let Some(v) = get_str(srv, "clientName") {
            s.server.client_name = v;
        }
        if let Some(v) = get_str(srv, "clientSecret") {
            s.server.client_secret = v;
        }
        if let Some(v) = get_str(srv, "exam") {
            s.server.exam_id = v;
        }
        if let Some(v) = get_str(srv, "institution") {
            s.server.institution = v;
        }
        if let Some(v) = get_i64(srv, "pingInterval") {
            if v > 0 {
                s.server.ping_interval_ms = v as u64;
            }
        }
    }

    // Audio
    if let Some(v) = get_bool(e, "audioSetVolumeLevel") {
        s.audio.initialize_volume = v;
    }
    if let Some(v) = get_i64(e, "audioVolumeLevel") {
        s.audio.initial_volume = v.clamp(0, 100) as i32;
    }
    if let Some(v) = get_bool(e, "audioMute") {
        s.audio.mute_audio = v;
    }

    // Display (including displayAlwaysOn!)
    if let Some(v) = get_i64(e, "allowedDisplaysMaxNumber") {
        if v > 0 {
            s.display.allowed_displays = v as i32;
        }
    }
    if let Some(v) = get_bool(e, "displayAlwaysOn") {
        s.display.always_on = v;
    }
    if let Some(v) = get_bool(e, "allowedDisplaysIgnoreFailure") {
        s.display.ignore_error = v;
    }
    if let Some(v) = get_bool(e, "allowedDisplayBuiltinEnforce") {
        s.display.internal_display_only = v;
    }

    // Keyboard & Mouse (including ALL F1..F12 + enableInjected!)
    if let Some(v) = get_bool(e, "enableAltTab") {
        s.keyboard.allow_alt_tab = v;
    }
    if let Some(v) = get_bool(e, "enableAltF4") {
        s.keyboard.allow_alt_f4 = v;
    }
    if let Some(v) = get_bool(e, "enableAltEsc") {
        s.keyboard.allow_alt_esc = v;
    }
    if let Some(v) = get_bool(e, "enableCtrlEsc") {
        s.keyboard.allow_ctrl_esc = v;
    }
    if let Some(v) = get_bool(e, "enableEsc") {
        s.keyboard.allow_esc = v;
    }
    if let Some(v) = get_bool(e, "enableF1") {
        s.keyboard.allow_f1 = v;
    }
    if let Some(v) = get_bool(e, "enableF2") {
        s.keyboard.allow_f2 = v;
    }
    if let Some(v) = get_bool(e, "enableF3") {
        s.keyboard.allow_f3 = v;
    }
    if let Some(v) = get_bool(e, "enableF4") {
        s.keyboard.allow_f4 = v;
    }
    if let Some(v) = get_bool(e, "enableF5") {
        s.keyboard.allow_f5 = v;
    }
    if let Some(v) = get_bool(e, "enableF6") {
        s.keyboard.allow_f6 = v;
    }
    if let Some(v) = get_bool(e, "enableF7") {
        s.keyboard.allow_f7 = v;
    }
    if let Some(v) = get_bool(e, "enableF8") {
        s.keyboard.allow_f8 = v;
    }
    if let Some(v) = get_bool(e, "enableF9") {
        s.keyboard.allow_f9 = v;
    }
    if let Some(v) = get_bool(e, "enableF10") {
        s.keyboard.allow_f10 = v;
    }
    if let Some(v) = get_bool(e, "enableF11") {
        s.keyboard.allow_f11 = v;
    }
    if let Some(v) = get_bool(e, "enableF12") {
        s.keyboard.allow_f12 = v;
    }
    if let Some(v) = get_bool(e, "enableInjected") {
        s.keyboard.allow_injected = v;
    }
    if let Some(v) = get_bool(e, "enablePrintScreen") {
        s.keyboard.allow_print_screen = v;
    }
    if let Some(v) = get_bool(e, "enableStartMenu") {
        s.keyboard.allow_system_key = v;
    }
    if let Some(v) = get_bool(e, "enableRightMouse") {
        s.mouse.allow_right_button = v;
    }
    if let Some(v) = get_bool(e, "enableMiddleMouse") {
        s.mouse.allow_middle_button = v;
    }

    // Power Supply thresholds
    if let Some(v) = get_f64(e, "batteryChargeThresholdCritical") {
        if v > 0.0 && v <= 1.0 {
            s.power_supply.charge_threshold_critical = v;
        }
    }
    if let Some(v) = get_f64(e, "batteryChargeThresholdLow") {
        if v > 0.0 && v <= 1.0 {
            s.power_supply.charge_threshold_low = v;
        }
    }

    // User Interface
    if let Some(v) = get_bool(e, "showTaskBar") {
        s.user_interface.taskbar.enable_taskbar = v;
    }
    if let Some(v) = get_bool(e, "showSideMenu") {
        s.user_interface.action_center.enable_action_center = v;
    }
    if let Some(v) = get_bool(e, "audioControlEnabled") {
        s.user_interface.taskbar.show_audio = v;
        s.user_interface.action_center.show_audio = v;
    }
    if let Some(v) = get_bool(e, "showTime") {
        s.user_interface.taskbar.show_clock = v;
        s.user_interface.action_center.show_clock = v;
    }
    if let Some(v) = get_bool(e, "showInputLanguage") {
        s.user_interface.taskbar.show_keyboard_layout = v;
        s.user_interface.action_center.show_keyboard_layout = v;
    }
    if let Some(v) = get_bool(e, "allowWlan") {
        s.user_interface.taskbar.show_network = v;
        s.user_interface.action_center.show_network = v;
    }
    if let Some(v) = get_bool(e, "showApplicationLogButton") {
        s.user_interface.taskbar.show_application_log = v;
    }
    // Fixed parity with security_data_mapper.cpp:
    s.user_interface.action_center.show_application_log = s.security.allow_application_log;
    if !s.security.allow_application_log {
        s.user_interface.taskbar.show_application_log = false;
    }
    if let Some(v) = get_bool(e, "showQRVerifyButton") {
        s.user_interface.taskbar.show_verificator = v;
    }
    if let Some(v) = get_str(e, "lockScreenBackgroundColor") {
        if !v.trim().is_empty() {
            s.user_interface.lock_screen.background_color = v;
        }
    }

    // Applications (100% parity with application_data_mapper.cpp: os==1, blacklist override/dedup, signature)
    if let Some(SebValue::List(items)) = find_val(e, "prohibitedProcesses") {
        for item in items {
            if let SebValue::Map(m) = item {
                if let Some(os) = get_i64(m, "os") {
                    if os != 1 {
                        continue;
                    }
                }
                let exe = get_str(m, "executable").unwrap_or_default();
                let orig = get_str(m, "originalName").unwrap_or_default();
                if exe.trim().is_empty() {
                    continue;
                }
                // Remove prior duplicate (allows active=false to disable an earlier rule)
                s.applications.blacklist.retain(|existing| {
                    !(existing.executable_name.eq_ignore_ascii_case(&exe)
                        && existing.original_name.eq_ignore_ascii_case(&orig))
                });
                if get_bool(m, "active").unwrap_or(true) {
                    s.applications.blacklist.push(BlacklistApplication {
                        auto_terminate: get_bool(m, "strongKill").unwrap_or(false),
                        executable_name: exe,
                        original_name: orig,
                    });
                }
            }
        }
    }
    if let Some(SebValue::List(items)) = find_val(e, "permittedProcesses") {
        for item in items {
            if let SebValue::Map(m) = item {
                if let Some(os) = get_i64(m, "os") {
                    if os != 1 {
                        continue;
                    }
                }
                if !get_bool(m, "active").unwrap_or(true) {
                    continue;
                }
                let exe = get_str(m, "executable").unwrap_or_default();
                if exe.trim().is_empty() {
                    continue;
                }
                let mut args = Vec::new();
                if let Some(SebValue::List(arg_list)) = find_val(m, "arguments") {
                    for a in arg_list {
                        if let SebValue::Map(am) = a {
                            if get_bool(am, "active").unwrap_or(true) {
                                if let Some(arg) = get_str(am, "argument") {
                                    args.push(arg);
                                }
                            }
                        }
                    }
                }
                s.applications.whitelist.push(WhitelistApplication {
                    allow_custom_path: get_bool(m, "allowUserToChooseApp").unwrap_or(false),
                    allow_running: get_bool(m, "runInBackground").unwrap_or(false),
                    arguments: args,
                    auto_start: get_bool(m, "autostart").unwrap_or(false),
                    auto_terminate: get_bool(m, "strongKill").unwrap_or(false),
                    description: get_str(m, "description").unwrap_or_default(),
                    display_name: get_str(m, "title").unwrap_or_else(|| exe.clone()),
                    executable_name: exe,
                    executable_path: get_str(m, "path").unwrap_or_default(),
                    original_name: get_str(m, "originalName").unwrap_or_default(),
                    show_in_shell: get_bool(m, "iconInTaskbar").unwrap_or(true),
                    signature: get_str(m, "signature").unwrap_or_default(),
                });
            }
        }
    }
}

fn parse_single_version_token(raw: &str) -> Option<VersionRestriction> {
    let t = raw.trim();
    if t.is_empty() {
        return None;
    }
    let parts: Vec<&str> = t.split('.').collect();
    // Standard SEB format: "win.3.7.0.min" / "win.3.7.AE"
    if parts.len() >= 3 && parts[0].eq_ignore_ascii_case("win") {
        let major = parts[1].parse::<i32>().ok()?;
        let minor = parts[2].parse::<i32>().ok()?;
        let patch = parts.get(3).and_then(|p| p.parse::<i32>().ok());
        let build = parts.get(4).and_then(|p| p.parse::<i32>().ok());
        let is_min = parts.iter().any(|p| p.eq_ignore_ascii_case("min"));
        let alliance = parts.iter().any(|p| p.eq_ignore_ascii_case("AE"));
        return Some(VersionRestriction {
            major,
            minor,
            patch,
            build,
            is_minimum_restriction: is_min,
            requires_alliance_edition: alliance,
        });
    }
    // Legacy/fallback string format ("Linux_3.7.0_min" / "3.7.0")
    let body = t
        .strip_prefix("Linux_")
        .or_else(|| t.strip_prefix("Win_"))
        .unwrap_or(t);
    let is_min = body.contains("_min");
    let alliance = body.contains("AE");
    let clean = body.replace("_min", "").replace("AE", "");
    let nums: Vec<&str> = clean.trim().split('.').collect();
    if nums.len() >= 2 {
        let major = nums[0].parse::<i32>().ok()?;
        let minor = nums[1].parse::<i32>().ok()?;
        let patch = nums.get(2).and_then(|p| p.parse::<i32>().ok());
        let build = nums.get(3).and_then(|p| p.parse::<i32>().ok());
        return Some(VersionRestriction {
            major,
            minor,
            patch,
            build,
            is_minimum_restriction: is_min,
            requires_alliance_edition: alliance,
        });
    }
    None
}

fn parse_version_restrictions_value(val: &SebValue) -> Vec<VersionRestriction> {
    let mut out = Vec::new();
    match val {
        SebValue::List(items) => {
            for item in items {
                if let SebValue::String(s) = item {
                    if let Some(vr) = parse_single_version_token(s) {
                        out.push(vr);
                    }
                }
            }
        }
        SebValue::String(raw) => {
            for token in raw.split(|c| c == ',' || c == ';' || c == '\n') {
                if let Some(vr) = parse_single_version_token(token) {
                    out.push(vr);
                }
            }
        }
        _ => {}
    }
    out
}

// =========================================================================
// Minimal JSON Config Loader (examples/minimal-config.json)
// =========================================================================

fn parse_json_settings(source: &[u8]) -> LoadedConfiguration {
    let val: serde_json::Value = match serde_json::from_slice(source) {
        Ok(v) => v,
        Err(_) => {
            return LoadedConfiguration::err(LoadStatus::InvalidData, "Invalid JSON configuration.")
        }
    };
    let mut s = AppSettings::default();
    if let Some(b) = val.get("browser").and_then(|v| v.as_object()) {
        if let Some(u) = b.get("startUrl").and_then(|v| v.as_str()) {
            s.browser.start_url = u.to_string();
        }
        if let Some(u) = b.get("homeUrl").and_then(|v| v.as_str()) {
            s.browser.home_url = u.to_string();
        }
        if let Some(u) = b.get("quitUrl").and_then(|v| v.as_str()) {
            s.browser.quit_url = u.to_string();
        }
        if let Some(v) = b.get("confirmQuitUrl").and_then(|v| v.as_bool()) {
            s.browser.confirm_quit_url = v;
        }
        if let Some(v) = b.get("allowDownloads").and_then(|v| v.as_bool()) {
            s.browser.allow_downloads = v;
        }
        if let Some(v) = b.get("allowUploads").and_then(|v| v.as_bool()) {
            s.browser.allow_uploads = v;
        }
        if let Some(v) = b.get("sendConfigurationKey").and_then(|v| v.as_bool()) {
            s.browser.send_configuration_key = v;
        }
        if let Some(v) = b.get("sendBrowserExamKey").and_then(|v| v.as_bool()) {
            s.browser.send_browser_exam_key = v;
        }
        if let Some(salt_b64) = b.get("browserExamKeySalt").and_then(|v| v.as_str()) {
            if let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(salt_b64) {
                s.browser.exam_key_salt = decoded;
            }
        }
        if let Some(f) = b.get("filter").and_then(|v| v.as_object()) {
            if let Some(v) = f.get("processMainRequests").and_then(|v| v.as_bool()) {
                s.browser.filter.process_main_requests = v;
            }
            if let Some(v) = f.get("processContentRequests").and_then(|v| v.as_bool()) {
                s.browser.filter.process_content_requests = v;
            }
            if let Some(arr) = f.get("rules").and_then(|v| v.as_array()) {
                for r in arr {
                    let expr = r
                        .get("expression")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    if expr.is_empty() {
                        continue;
                    }
                    let allow = matches!(r.get("result").and_then(|v| v.as_str()), Some("allow"));
                    let is_regex = matches!(r.get("type").and_then(|v| v.as_str()), Some("regex"));
                    s.browser.filter.rules.push(FilterRuleConfig {
                        active: true,
                        expression: expr,
                        is_regex,
                        allow,
                    });
                }
            }
        }
        if let Some(mw) = b.get("mainWindow").and_then(|v| v.as_object()) {
            if let Some(v) = mw.get("showToolbar").and_then(|v| v.as_bool()) {
                s.browser.main_window.show_toolbar = v;
            }
            if let Some(v) = mw.get("allowAddressBar").and_then(|v| v.as_bool()) {
                s.browser.main_window.allow_address_bar = v;
            }
            if let Some(v) = mw.get("allowReloading").and_then(|v| v.as_bool()) {
                s.browser.main_window.allow_reloading = v;
            }
            if let Some(v) = mw.get("showReloadButton").and_then(|v| v.as_bool()) {
                s.browser.main_window.show_reload_button = v;
            }
            if let Some(v) = mw.get("alwaysOnTop").and_then(|v| v.as_bool()) {
                s.browser.main_window.always_on_top = v;
            }
            if let Some(v) = mw.get("allowMinimize").and_then(|v| v.as_bool()) {
                s.browser.main_window.allow_minimize = v;
            }
        }
    }
    if let Some(sec) = val.get("security").and_then(|v| v.as_object()) {
        if let Some(v) = sec.get("allowTermination").and_then(|v| v.as_bool()) {
            s.security.allow_termination = v;
        }
    }
    process_settings_default(&mut s);
    s.browser.configuration_key = sha256_hex(source);
    LoadedConfiguration {
        status: LoadStatus::Success,
        settings: s,
        parsed_plist: None,
        raw_seb_data: Some(source.to_vec()),
        used_password_hash: None,
        redirect_url: None,
        message: String::new(),
    }
}
