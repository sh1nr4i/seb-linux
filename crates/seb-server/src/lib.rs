pub mod client;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerApi {
    pub access_token_endpoint: String,
    pub configuration_endpoint: String,
    pub handshake_endpoint: String,
    pub log_endpoint: String,
    pub ping_endpoint: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExamInfo {
    pub id: String,
    pub lms_name: String,
    pub name: String,
    pub url: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProctoringInstructionData {
    /// 0=Unknown, 1=JitsiMeet, 2=Zoom, 3=ScreenProctoring
    pub provider: i32,
    /// 0=Unknown, 1=Join, 2=Leave
    pub method: i32,
    pub client_id: String,
    pub client_secret: String,
    pub group_id: String,
    pub service_url: String,
    pub session_id: String,
    pub encryption_secret: String,
    pub room_name: String,
    pub token: String,
    pub meeting_number: String,
    pub password: String,
    pub sdk_key: String,
    pub signature: String,
    pub subject: String,
    pub user_name: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstructionData {
    /// 0=None, 1=LockScreen, 2=NotificationConfirm, 3=Proctoring, 4=ProctoringReconfiguration, 5=Quit
    pub instruction: i32,
    /// 0=Unknown, 1=LockScreen, 2=Hand
    pub notification_type: i32,
    pub message: String,
    pub confirmation: String,
    pub notification_id: i32,
    pub allow_chat: bool,
    pub receive_audio: bool,
    pub receive_video: bool,
    pub proctoring: ProctoringInstructionData,
}

pub fn base_address(server_url: &str) -> Option<String> {
    let mut url = Url::parse(server_url.trim()).ok()?;
    let scheme = url.scheme();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    if url.host_str().unwrap_or("").is_empty()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.query().is_some()
    {
        return None;
    }
    if !url.path().ends_with('/') {
        let mut p = url.path().to_string();
        p.push('/');
        url.set_path(&p);
    }
    Some(url.to_string())
}

pub fn api_endpoint(endpoint: &str) -> String {
    let value = endpoint.trim();
    if let Ok(abs) = Url::parse(value) {
        if !abs.scheme().is_empty() {
            return value.to_string();
        }
    }
    value.strip_prefix('/').unwrap_or(value).to_string()
}

pub fn resolve_url(base_url: &str, endpoint: &str) -> Option<String> {
    let trimmed = endpoint.trim();
    if trimmed.is_empty() || trimmed.starts_with("//") {
        return None;
    }
    let base = Url::parse(base_url).ok()?;
    let normalized = api_endpoint(trimmed);
    let resolved = base.join(&normalized).ok()?;

    if !resolved.username().is_empty()
        || resolved.password().is_some()
        || resolved.fragment().is_some()
        || resolved.scheme() != base.scheme()
    {
        return None;
    }
    let b_host = base.host_str()?;
    let r_host = resolved.host_str()?;
    if !r_host.eq_ignore_ascii_case(b_host) {
        return None;
    }
    if resolved.port_or_known_default() != base.port_or_known_default() {
        return None;
    }
    Some(resolved.to_string())
}

fn parse_json_object(body: &[u8]) -> Result<serde_json::Map<String, Value>, String> {
    let val: Value = serde_json::from_slice(body).map_err(|e| e.to_string())?;
    match val {
        Value::Object(map) => Ok(map),
        _ => Err("Expected a JSON object from SEB Server.".to_string()),
    }
}

pub fn parse_api(body: &[u8]) -> Result<ServerApi, String> {
    let root = parse_json_object(body)?;
    if let Some(versions) = root.get("api-versions").and_then(|v| v.as_array()) {
        for ver in versions {
            if ver.get("name").and_then(|v| v.as_str()) != Some("v1") {
                continue;
            }
            let mut api = ServerApi::default();
            if let Some(endpoints) = ver.get("endpoints").and_then(|v| v.as_array()) {
                for ep in endpoints {
                    let name = ep.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    let loc = ep
                        .get("location")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    match name {
                        "access-token-endpoint" => api.access_token_endpoint = loc,
                        "seb-configuration-endpoint" => api.configuration_endpoint = loc,
                        "seb-handshake-endpoint" => api.handshake_endpoint = loc,
                        "seb-log-endpoint" => api.log_endpoint = loc,
                        "seb-ping-endpoint" => api.ping_endpoint = loc,
                        _ => {}
                    }
                }
            }
            if !api.access_token_endpoint.is_empty()
                && !api.configuration_endpoint.is_empty()
                && !api.handshake_endpoint.is_empty()
                && !api.ping_endpoint.is_empty()
            {
                return Ok(api);
            }
        }
    }
    Err("The server does not advertise a complete SEB Server API v1.".to_string())
}

pub fn parse_oauth_token(body: &[u8]) -> Result<String, String> {
    let root = parse_json_object(body)?;
    let token = root
        .get("access_token")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if token.is_empty() {
        return Err("The server did not return an OAuth access token.".to_string());
    }
    Ok(token)
}

pub fn parse_exams(body: &[u8]) -> Result<Vec<ExamInfo>, String> {
    let val: Value =
        serde_json::from_slice(body).map_err(|_| "Malformed SEB Server exam list.".to_string())?;
    let Some(arr) = val.as_array() else {
        return Err("Malformed SEB Server exam list.".to_string());
    };
    let mut exams = Vec::new();
    for item in arr {
        let id = item
            .get("examId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let lms_name = item
            .get("lmsType")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let name = item
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let url = item
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if !id.is_empty() && !name.is_empty() && !url.is_empty() {
            exams.push(ExamInfo {
                id,
                lms_name,
                name,
                url,
            });
        }
    }
    if exams.is_empty() {
        return Err("The server returned no usable exams.".to_string());
    }
    Ok(exams)
}

fn str_attr(attrs: &serde_json::Map<String, Value>, key: &str) -> String {
    attrs
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

pub fn parse_instruction(body: &[u8]) -> Result<InstructionData, String> {
    let root = parse_json_object(body)?;
    let name = root.get("instruction").and_then(|v| v.as_str()).unwrap_or("");
    let instruction_code = match name {
        "SEB_FORCE_LOCK_SCREEN" => 1,
        "NOTIFICATION_CONFIRM" => 2,
        "SEB_PROCTORING" => 3,
        "SEB_RECONFIGURE_SETTINGS" => 4,
        "SEB_QUIT" => 5,
        _ => return Err("Unknown or empty instruction".to_string()),
    };

    let empty_map = serde_json::Map::new();
    let attrs = match root.get("attributes") {
        Some(Value::Object(m)) => m,
        Some(_) => return Err("Invalid attributes object".to_string()),
        None => &empty_map,
    };

    for field in ["instruction-confirm", "message", "type"] {
        if let Some(v) = attrs.get(field) {
            if !v.is_string() {
                return Err(format!("Invalid attribute field {field}"));
            }
        }
    }

    let mut out = InstructionData {
        instruction: instruction_code,
        confirmation: str_attr(attrs, "instruction-confirm"),
        message: str_attr(attrs, "message"),
        notification_id: attrs
            .get("id")
            .and_then(|v| {
                v.as_i64()
                    .or_else(|| v.as_str().and_then(|s| s.parse::<i64>().ok()))
            })
            .unwrap_or(0) as i32,
        allow_chat: attrs
            .get("jitsiMeetFeatureFlagChat")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
            || attrs
                .get("zoomFeatureFlagChat")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        receive_audio: attrs
            .get("jitsiMeetReceiveAudio")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
            || attrs
                .get("zoomReceiveAudio")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        receive_video: attrs
            .get("jitsiMeetReceiveVideo")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
            || attrs
                .get("zoomReceiveVideo")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        ..Default::default()
    };

    let ntype = str_attr(attrs, "type");
    if ntype.eq_ignore_ascii_case("lockscreen") {
        out.notification_type = 1;
    } else if ntype.eq_ignore_ascii_case("raisehand") || ntype.eq_ignore_ascii_case("hand") {
        out.notification_type = 2;
    }

    if instruction_code == 2 {
        let valid_id = match attrs.get("id") {
            Some(Value::Number(n)) => n.as_i64().map_or(false, |v| v > 0),
            Some(Value::String(s)) => s.parse::<i64>().map_or(false, |v| v > 0),
            _ => false,
        };
        if !valid_id || out.notification_type == 0 {
            return Err("Invalid notification confirm attributes".to_string());
        }
    }

    if instruction_code == 3 {
        let provider_str = str_attr(attrs, "service-type");
        let method_str = str_attr(attrs, "method");
        let mut p = ProctoringInstructionData {
            provider: match provider_str.as_str() {
                "JITSI_MEET" => 1,
                "ZOOM" => 2,
                "SCREEN_PROCTORING" => 3,
                _ => 0,
            },
            method: match method_str.as_str() {
                "JOIN" => 1,
                "LEAVE" => 2,
                _ => 0,
            },
            client_id: str_attr(attrs, "screenProctoringClientId"),
            client_secret: str_attr(attrs, "screenProctoringClientSecret"),
            group_id: str_attr(attrs, "screenProctoringGroupId"),
            service_url: str_attr(attrs, "screenProctoringServiceURL"),
            session_id: str_attr(attrs, "screenProctoringClientSessionId"),
            encryption_secret: str_attr(attrs, "screenProctoringEncryptSecret"),
            room_name: str_attr(attrs, "jitsiMeetRoom"),
            token: str_attr(attrs, "jitsiMeetToken"),
            meeting_number: str_attr(attrs, "zoomRoom"),
            password: str_attr(attrs, "zoomMeetingKey"),
            sdk_key: str_attr(attrs, "zoomAPIKey"),
            signature: str_attr(attrs, "zoomToken"),
            subject: str_attr(attrs, "zoomSubject"),
            user_name: str_attr(attrs, "zoomUserName"),
        };
        if p.provider == 1 {
            p.service_url = str_attr(attrs, "jitsiMeetServerURL");
        }
        out.proctoring = p;
    }

    Ok(out)
}

pub fn is_token_expired(body: &[u8]) -> bool {
    parse_json_object(body)
        .ok()
        .and_then(|m| {
            m.get("error")
                .and_then(|v| v.as_str())
                .map(|s| s.eq_ignore_ascii_case("invalid_token"))
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizer_blocks_cross_origin_and_resolves_same_origin() {
        let base = base_address("https://seb.example.org/admin").unwrap();
        assert_eq!(base, "https://seb.example.org/admin/");
        assert_eq!(
            resolve_url(&base, "/oauth/token").unwrap(),
            "https://seb.example.org/admin/oauth/token"
        );
        assert!(resolve_url(&base, "https://evil.example.org/steal").is_none());
        assert!(resolve_url(&base, "//evil.example.org/steal").is_none());
    }

    #[test]
    fn parses_screen_proctoring_instruction() {
        let raw = br#"{"instruction":"SEB_PROCTORING","attributes":{"service-type":"SCREEN_PROCTORING","method":"JOIN","screenProctoringServiceURL":"https://sps.example.org","instruction-confirm":"42"}}"#;
        let parsed = parse_instruction(raw).unwrap();
        assert_eq!(parsed.instruction, 3);
        assert_eq!(parsed.proctoring.provider, 3);
        assert_eq!(parsed.proctoring.method, 1);
        assert_eq!(parsed.proctoring.service_url, "https://sps.example.org");
        assert_eq!(parsed.confirmation, "42");
    }
}
