use crate::{
    api_endpoint, base_address, is_token_expired, parse_api, parse_exams, parse_instruction,
    parse_oauth_token, resolve_url, ExamInfo, InstructionData, ServerApi,
};
use base64::Engine;
use std::collections::VecDeque;
use std::io::Read;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
pub struct ServerConnectionConfig {
    pub server_url: String,
    pub api_url: String,
    pub client_name: String,
    pub client_secret: String,
    pub institution: String,
    pub exam_id: String,
    pub request_attempts: u32,
    pub request_attempt_interval_ms: u64,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone)]
pub struct HandshakeResult {
    pub connection_token: String,
    pub exams: Vec<ExamInfo>,
}

#[derive(Debug, Clone)]
pub struct SelectExamResult {
    pub custom_browser_exam_key: Option<String>,
}

#[derive(Debug, Clone)]
pub enum PingOutcome {
    Instruction(InstructionData),
    OkEmpty,
    TerminatedByServer(u16),
}

pub struct SebServerClient {
    cfg: ServerConnectionConfig,
    agent: ureq::Agent,
    base_url: String,
    api: ServerApi,
    oauth_token: String,
    connection_token: String,
    selected_exam_id: String,
    ping_count: u64,
    confirm_queue: VecDeque<String>,
    next_notification_id: i32,
    current_hand_id: Option<i32>,
    current_lock_screen_id: Option<i32>,
    last_sent_user_id: String,
    last_wireless_signal: Option<i32>,
}

impl SebServerClient {
    pub fn new(cfg: ServerConnectionConfig) -> Result<Self, String> {
        let base_url = base_address(&cfg.server_url)
            .ok_or_else(|| "Invalid SEB Server URL.".to_string())?;
        let timeout = Duration::from_millis(cfg.timeout_ms.max(3000));
        // Disable redirects to match C++ ManualRedirectPolicy!
        let agent = ureq::AgentBuilder::new()
            .timeout(timeout)
            .redirects(0)
            .build();
        Ok(Self {
            cfg,
            agent,
            base_url,
            api: ServerApi::default(),
            oauth_token: String::new(),
            connection_token: String::new(),
            selected_exam_id: String::new(),
            ping_count: 0,
            confirm_queue: VecDeque::new(),
            next_notification_id: 1,
            current_hand_id: None,
            current_lock_screen_id: None,
            last_sent_user_id: String::new(),
            last_wireless_signal: None,
        })
    }

    pub fn discover_api(&mut self) -> Result<ServerApi, String> {
        let endpoint = api_endpoint(&self.cfg.api_url);
        let url = resolve_url(&self.base_url, &endpoint)
            .ok_or_else(|| "Unable to resolve SEB Server API endpoint.".to_string())?;
        let (_, body) = self.execute_with_retry("GET", &url, None, None, false, false)?;
        let api = parse_api(&body)?;
        // Validate all 5 discovered endpoints stay same-origin (C++ ServerProxy::initialize)
        for ep in [
            &api.access_token_endpoint,
            &api.configuration_endpoint,
            &api.handshake_endpoint,
            &api.log_endpoint,
            &api.ping_endpoint,
        ] {
            if !ep.is_empty() && resolve_url(&self.base_url, ep).is_none() {
                return Err(format!("SEB Server returned unsafe cross-origin endpoint: {ep}"));
            }
        }
        self.api = api.clone();
        Ok(api)
    }

    pub fn authenticate(&mut self) -> Result<String, String> {
        let ep = if self.api.access_token_endpoint.is_empty() {
            "/oauth/token"
        } else {
            &self.api.access_token_endpoint
        };
        let url = resolve_url(&self.base_url, ep)
            .ok_or_else(|| "Invalid OAuth token endpoint.".to_string())?;
        let creds = base64::engine::general_purpose::STANDARD
            .encode(format!("{}:{}", self.cfg.client_name, self.cfg.client_secret));
        let attempts = self.cfg.request_attempts.max(1);
        let interval = Duration::from_millis(self.cfg.request_attempt_interval_ms.max(250));
        let mut last_err = String::from("SEB Server OAuth failed.");
        for attempt in 0..attempts {
            match self
                .agent
                .post(&url)
                .set("Authorization", &format!("Basic {creds}"))
                .set("Content-Type", "application/x-www-form-urlencoded")
                .set("Accept", "application/json")
                .send_string("grant_type=client_credentials&scope=read%20write")
            {
                Ok(resp) => {
                    let mut body = Vec::new();
                    resp.into_reader()
                        .take(1024 * 1024)
                        .read_to_end(&mut body)
                        .map_err(|e| e.to_string())?;
                    let token = parse_oauth_token(&body)?;
                    self.oauth_token = token.clone();
                    return Ok(token);
                }
                Err(ureq::Error::Status(code, _)) => {
                    last_err = format!("SEB Server OAuth HTTP {code}");
                    let retryable = code == 408 || code == 429 || code >= 500;
                    if !retryable || attempt + 1 >= attempts {
                        break;
                    }
                    thread::sleep(interval);
                }
                Err(e) => {
                    last_err = format!("SEB Server OAuth failed: {e}");
                    if attempt + 1 >= attempts {
                        break;
                    }
                    thread::sleep(interval);
                }
            }
        }
        Err(last_err)
    }

    pub fn handshake(&mut self, os_name: &str, seb_version: &str) -> Result<HandshakeResult, String> {
        let ep = if self.api.handshake_endpoint.is_empty() {
            "/sebapi/v1/handshake"
        } else {
            &self.api.handshake_endpoint
        };
        let url = resolve_url(&self.base_url, ep)
            .ok_or_else(|| "Invalid handshake endpoint.".to_string())?;
        let client_id = std::env::var("USER").unwrap_or_else(|_| "unknown".to_string());
        let machine_name = read_hostname();
        let mut form = format!(
            "institutionId={}&client_id={}&seb_machine_name={}&seb_os_name={}&seb_version={}",
            url_encode(&self.cfg.institution),
            url_encode(&client_id),
            url_encode(&machine_name),
            url_encode(os_name),
            url_encode(seb_version),
        );
        if !self.cfg.exam_id.trim().is_empty() {
            form.push_str(&format!("&examId={}", url_encode(self.cfg.exam_id.trim())));
        }

        let (headers, body) = self.execute_with_retry(
            "POST",
            &url,
            Some("application/x-www-form-urlencoded"),
            Some(form.as_bytes()),
            true,
            false,
        )?;
        let conn_token = header_ci(&headers, "SEBConnectionToken").unwrap_or_default();
        if conn_token.is_empty() {
            return Err("SEB Server handshake did not return a SEBConnectionToken.".to_string());
        }
        let exams = parse_exams(&body)?;
        self.connection_token = conn_token.clone();
        Ok(HandshakeResult {
            connection_token: conn_token,
            exams,
        })
    }

    pub fn fetch_exam_configuration(&mut self, exam_id: &str) -> Result<Vec<u8>, String> {
        let ep = if self.api.configuration_endpoint.is_empty() {
            "/sebapi/v1/examconfig"
        } else {
            &self.api.configuration_endpoint
        };
        let url = resolve_url(&self.base_url, ep)
            .ok_or_else(|| "Invalid configuration endpoint.".to_string())?;
        let full = if exam_id.is_empty() {
            url
        } else {
            format!("{url}?examId={}", url_encode(exam_id))
        };
        let (_, body) = self.execute_with_retry("GET", &full, None, None, true, true)?;
        Ok(body)
    }

    /// 100% parity with C++ `ServerProxy::selectExam`:
    /// 1. PATCH {handshake} with examId -> read SEBServerBEK & SEBExamSalt headers
    /// 2. PUT {handshake} to finalize exam connection
    pub fn select_exam(&mut self, exam_id: &str) -> Result<SelectExamResult, String> {
        let ep = if self.api.handshake_endpoint.is_empty() {
            "/sebapi/v1/handshake"
        } else {
            &self.api.handshake_endpoint
        };
        let url = resolve_url(&self.base_url, ep)
            .ok_or_else(|| "Invalid handshake endpoint.".to_string())?;
        let patch_body = format!("examId={}", url_encode(exam_id));
        let (headers, _) = self.execute_with_retry(
            "PATCH",
            &url,
            Some("application/x-www-form-urlencoded"),
            Some(patch_body.as_bytes()),
            true,
            true,
        )?;

        let server_bek = header_ci(&headers, "SEBServerBEK").filter(|s| !s.is_empty());
        if let Some(salt) = header_ci(&headers, "SEBExamSalt").filter(|s| !s.is_empty()) {
            let _ = salt;
            return Err(
                "This server requires an App Signature Key; Linux protected integrity is unsupported."
                    .to_string(),
            );
        }

        let _ = self.execute_with_retry(
            "PUT",
            &url,
            Some("application/x-www-form-urlencoded"),
            Some(b""),
            true,
            true,
        )?;
        self.selected_exam_id = exam_id.to_string();
        Ok(SelectExamResult {
            custom_browser_exam_key: server_bek,
        })
    }

    pub fn send_user_identifier(&mut self, user_id: &str) -> Result<(), String> {
        let clean = user_id.trim();
        if clean.is_empty() || clean == self.last_sent_user_id || self.connection_token.is_empty() {
            return Ok(());
        }
        let ep = if self.api.handshake_endpoint.is_empty() {
            "/sebapi/v1/handshake"
        } else {
            &self.api.handshake_endpoint
        };
        let url = resolve_url(&self.base_url, ep)
            .ok_or_else(|| "Invalid handshake endpoint.".to_string())?;
        let body = format!(
            "examId={}&seb_user_session_id={}",
            url_encode(&self.selected_exam_id),
            url_encode(clean)
        );
        self.execute_with_retry(
            "PATCH",
            &url,
            Some("application/x-www-form-urlencoded"),
            Some(body.as_bytes()),
            true,
            true,
        )?;
        self.last_sent_user_id = clean.to_string();
        Ok(())
    }

    pub fn enqueue_confirmation(&mut self, token: &str) {
        let t = token.trim();
        if !t.is_empty() && !self.confirm_queue.iter().any(|x| x == t) {
            self.confirm_queue.push_back(t.to_string());
        }
    }

    pub fn send_ping(&mut self) -> Result<PingOutcome, String> {
        let ep = if self.api.ping_endpoint.is_empty() {
            "/sebapi/v1/sebping"
        } else {
            &self.api.ping_endpoint
        };
        let url = resolve_url(&self.base_url, ep)
            .ok_or_else(|| "Invalid ping endpoint.".to_string())?;
        self.ping_count += 1;
        let ts = now_ms();
        let mut form = format!("timestamp={ts}&ping-number={}", self.ping_count);
        let confirming = self.confirm_queue.front().cloned();
        if let Some(ref tok) = confirming {
            form.push_str(&format!("&instruction-confirm={}", url_encode(tok)));
        }

        match self.execute_once(
            "POST",
            &url,
            Some("application/x-www-form-urlencoded"),
            Some(form.as_bytes()),
            true,
            true,
            true,
        ) {
            Ok((_, body)) => {
                if confirming.is_some() {
                    self.confirm_queue.pop_front();
                }
                if body.is_empty() {
                    return Ok(PingOutcome::OkEmpty);
                }
                match parse_instruction(&body) {
                    Ok(inst) => Ok(PingOutcome::Instruction(inst)),
                    Err(_) => Ok(PingOutcome::OkEmpty),
                }
            }
            Err((status, msg)) => {
                if (400..500).contains(&status) && status != 408 && status != 429 {
                    return Ok(PingOutcome::TerminatedByServer(status));
                }
                Err(msg)
            }
        }
    }

    pub fn raise_hand(&mut self, message: &str) -> Result<i32, String> {
        let id = self.next_notification_id;
        self.next_notification_id += 1;
        let payload = serde_json::json!({
            "numericValue": id,
            "text": format!("<raisehand> {}", message.trim()),
            "timestamp": now_ms(),
            "type": "NOTIFICATION",
        });
        self.post_log_json(&payload)?;
        self.current_hand_id = Some(id);
        Ok(id)
    }

    pub fn lower_hand(&mut self) -> Result<(), String> {
        let id = self.current_hand_id.unwrap_or(0);
        let payload = serde_json::json!({
            "numericValue": id,
            "timestamp": now_ms(),
            "type": "NOTIFICATION_CONFIRMED",
        });
        self.post_log_json(&payload)?;
        self.current_hand_id = None;
        Ok(())
    }

    pub fn current_hand_id(&self) -> Option<i32> {
        self.current_hand_id
    }

    pub fn clear_hand_if_matches(&mut self, id: i32) -> bool {
        if self.current_hand_id == Some(id) {
            self.current_hand_id = None;
            true
        } else {
            false
        }
    }

    pub fn send_lock_screen_notification(&mut self, message: &str) -> Result<i32, String> {
        let id = self.next_notification_id;
        self.next_notification_id += 1;
        let payload = serde_json::json!({
            "numericValue": id,
            "text": format!("<lockscreen> {}", message.trim()),
            "timestamp": now_ms(),
            "type": "NOTIFICATION",
        });
        self.post_log_json(&payload)?;
        self.current_lock_screen_id = Some(id);
        Ok(id)
    }

    pub fn confirm_lock_screen(&mut self) -> Result<(), String> {
        let id = self.current_lock_screen_id.unwrap_or(0);
        let payload = serde_json::json!({
            "numericValue": id,
            "timestamp": now_ms(),
            "type": "NOTIFICATION_CONFIRMED",
        });
        self.post_log_json(&payload)?;
        Ok(())
    }

    pub fn clear_lock_screen_if_matches(&mut self, id: i32) -> bool {
        if self.current_lock_screen_id == Some(id) {
            self.current_lock_screen_id = None;
            true
        } else {
            false
        }
    }

    pub fn log_event(&mut self, message: &str, severity: &str) {
        if self.connection_token.is_empty() {
            return;
        }
        let log_type = match severity.to_ascii_uppercase().as_str() {
            "ERROR" | "CRITICAL" => "ERROR_LOG",
            "WARN" | "WARNING" => "WARN_LOG",
            "DEBUG" => "DEBUG_LOG",
            _ => "INFO_LOG",
        };
        let sanitized = self.redact(message);
        let truncated: String = sanitized.chars().take(2048).collect();
        let payload = serde_json::json!({
            "text": truncated,
            "timestamp": now_ms(),
            "type": log_type,
        });
        let _ = self.post_log_json(&payload);
    }

    pub fn report_battery(&mut self, percentage: i32, power_connected: bool) {
        if self.connection_token.is_empty() {
            return;
        }
        let state = if power_connected {
            "connected"
        } else {
            "disconnected"
        };
        let payload = serde_json::json!({
            "numericValue": percentage,
            "text": format!("<battery> {percentage}% ({state})"),
            "timestamp": now_ms(),
            "type": "INFO_LOG",
        });
        let _ = self.post_log_json(&payload);
    }

    pub fn report_wireless(&mut self, signal_percent: Option<i32>) {
        if self.connection_token.is_empty() {
            return;
        }
        if let (Some(prev), Some(curr)) = (self.last_wireless_signal, signal_percent) {
            if (prev - curr).abs() < 5 {
                return;
            }
        } else if self.last_wireless_signal == signal_percent {
            return;
        }
        self.last_wireless_signal = signal_percent;
        let payload = match signal_percent {
            Some(sig) => serde_json::json!({
                "numericValue": sig,
                "text": format!("<wlan> {sig}%"),
                "timestamp": now_ms(),
                "type": "INFO_LOG",
            }),
            None => serde_json::json!({
                "text": "<wlan> disconnected",
                "timestamp": now_ms(),
                "type": "INFO_LOG",
            }),
        };
        let _ = self.post_log_json(&payload);
    }

    /// Returns true if server disconnect succeeded (matches C++ `disconnectFromServer` with `delete=true`).
    pub fn disconnect(&mut self) -> bool {
        if self.connection_token.is_empty() {
            return true;
        }
        let ep = if self.api.handshake_endpoint.is_empty() {
            "/sebapi/v1/handshake"
        } else {
            &self.api.handshake_endpoint
        };
        let ok = if let Some(url) = resolve_url(&self.base_url, ep) {
            self.execute_once(
                "DELETE",
                &url,
                Some("application/x-www-form-urlencoded"),
                Some(b"delete=true"),
                true,
                true,
                true,
            )
            .is_ok()
        } else {
            false
        };
        self.connection_token.clear();
        self.oauth_token.clear();
        self.cfg.client_secret.clear();
        ok
    }

    fn post_log_json(&mut self, payload: &serde_json::Value) -> Result<(), String> {
        let ep = if self.api.log_endpoint.is_empty() {
            "/sebapi/v1/seblog"
        } else {
            &self.api.log_endpoint
        };
        let url = resolve_url(&self.base_url, ep)
            .ok_or_else(|| "Invalid log endpoint.".to_string())?;
        let text = payload.to_string();
        self.execute_once(
            "POST",
            &url,
            Some("application/json;charset=UTF-8"),
            Some(text.as_bytes()),
            true,
            true,
            true,
        )
        .map(|_| ())
        .map_err(|(_, m)| m)
    }

    fn redact(&self, input: &str) -> String {
        let mut out = input.to_string();
        for secret in [
            self.oauth_token.as_str(),
            self.connection_token.as_str(),
            self.cfg.client_secret.as_str(),
        ] {
            if !secret.is_empty() {
                out = out.replace(secret, "[redacted]");
            }
        }
        out
    }

    fn execute_with_retry(
        &mut self,
        method: &str,
        url: &str,
        content_type: Option<&str>,
        body: Option<&[u8]>,
        use_bearer: bool,
        use_conn_token: bool,
    ) -> Result<(Vec<(String, String)>, Vec<u8>), String> {
        let attempts = self.cfg.request_attempts.max(1);
        let interval = Duration::from_millis(self.cfg.request_attempt_interval_ms.max(250));
        let mut last_err = String::from("SEB Server request failed.");
        for attempt in 0..attempts {
            match self.execute_once(method, url, content_type, body, use_bearer, use_conn_token, true)
            {
                Ok(ok) => return Ok(ok),
                Err((status, msg)) => {
                    last_err = msg;
                    let retryable = status == 0 || status == 408 || status == 429 || status >= 500;
                    if !retryable || attempt + 1 >= attempts {
                        break;
                    }
                    thread::sleep(interval);
                }
            }
        }
        Err(last_err)
    }

    #[allow(clippy::too_many_arguments)]
    fn execute_once(
        &mut self,
        method: &str,
        url: &str,
        content_type: Option<&str>,
        body: Option<&[u8]>,
        use_bearer: bool,
        use_conn_token: bool,
        allow_refresh: bool,
    ) -> Result<(Vec<(String, String)>, Vec<u8>), (u16, String)> {
        let mut req = self.agent.request(method, url).set("Accept", "application/json, */*");
        if let Some(ct) = content_type {
            req = req.set("Content-Type", ct);
        }
        if use_bearer && !self.oauth_token.is_empty() {
            req = req.set("Authorization", &format!("Bearer {}", self.oauth_token));
        }
        if use_conn_token && !self.connection_token.is_empty() {
            req = req.set("SEBConnectionToken", &self.connection_token);
        }
        let resp_res = match body {
            Some(bytes) => req.send_bytes(bytes),
            None => req.call(),
        };
        match resp_res {
            Ok(resp) => {
                let hdrs: Vec<(String, String)> = resp
                    .headers_names()
                    .into_iter()
                    .filter_map(|name| {
                        resp.header(&name)
                            .map(|val| (name, val.trim().to_string()))
                    })
                    .collect();
                let mut out = Vec::new();
                let _ = resp.into_reader().take(32 * 1024 * 1024).read_to_end(&mut out);
                Ok((hdrs, out))
            }
            Err(ureq::Error::Status(401, resp)) if allow_refresh && use_bearer => {
                let mut err_body = Vec::new();
                let _ = resp.into_reader().take(64 * 1024).read_to_end(&mut err_body);
                if is_token_expired(&err_body) && self.authenticate().is_ok() {
                    return self.execute_once(
                        method,
                        url,
                        content_type,
                        body,
                        use_bearer,
                        use_conn_token,
                        false,
                    );
                }
                Err((401, "SEB Server authentication expired.".to_string()))
            }
            Err(ureq::Error::Status(code, _)) => Err((code, format!("SEB Server HTTP {code}"))),
            Err(e) => Err((0, format!("SEB Server network error: {e}"))),
        }
    }
}

/// Fetches Moodle user ID using the browser's `MoodleSession` cookie (matches C++ `lookupMoodleIdentity`).
pub fn lookup_moodle_user_id(origin: &str, cookie_name: &str, cookie_value: &str) -> Option<String> {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(8))
        .redirects(0)
        .build();
    let cookie_hdr = format!("{cookie_name}={cookie_value}");
    let clean_origin = origin.trim_end_matches('/');
    for path in [
        "/mod/quiz/accessrule/sebserver/classes/external/user.php",
        "/theme/boost_ethz/sebuser.php",
    ] {
        let url = format!("{clean_origin}{path}");
        if let Ok(resp) = agent.get(&url).set("Cookie", &cookie_hdr).call() {
            let mut body = String::new();
            if resp.into_reader().take(4096).read_to_string(&mut body).is_ok() {
                let trimmed = body.trim();
                if let Ok(uid) = trimmed.parse::<i64>() {
                    if uid > 0 {
                        return Some(uid.to_string());
                    }
                }
            }
        }
    }
    None
}

fn header_ci(headers: &[(String, String)], key: &str) -> Option<String> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v.clone())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn read_hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "linux".to_string())
}
