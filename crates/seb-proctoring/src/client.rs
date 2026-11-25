use crate::{
    build_metadata_json, process_and_encode_rgba, ImageFormatKind, ImageQuantizationKind,
    MetadataInput,
};
use base64::Engine;
use std::collections::VecDeque;
use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use url::Url;

const MAX_QUEUED_BYTES: usize = 128 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProctoringState {
    Inactive,
    Connecting,
    Active,
    Error,
}

#[derive(Debug, Clone)]
pub struct ProctoringClientConfig {
    pub service_url: String,
    pub client_id: String,
    pub client_secret: String,
    pub group_id: String,
    pub session_id: String,
    pub cache_size: usize,
    pub downscaling: f64,
    pub quantization: ImageQuantizationKind,
    pub format: ImageFormatKind,
}

struct CaptureFrame {
    timestamp_ms: u64,
    image_format: String,
    image_data: Vec<u8>,
    metadata_json: Vec<u8>,
}

struct QueueState {
    frames: VecDeque<CaptureFrame>,
    queued_bytes: usize,
    state: ProctoringState,
    status_text: String,
}

pub struct ScreenProctoringClient {
    cfg: ProctoringClientConfig,
    running: Arc<AtomicBool>,
    sent_count: Arc<AtomicU64>,
    shared: Arc<(Mutex<QueueState>, Condvar)>,
    log_rx: Mutex<Receiver<(String, String)>>,
    worker: Mutex<Option<thread::JoinHandle<()>>>,
}

impl ScreenProctoringClient {
    pub fn start(cfg: ProctoringClientConfig) -> Result<Self, String> {
        let base_url = normalize_service_url(&cfg.service_url)
            .ok_or_else(|| "Screen Proctoring could not start: invalid service URL.".to_string())?;
        let running = Arc::new(AtomicBool::new(true));
        let sent_count = Arc::new(AtomicU64::new(0));
        let shared = Arc::new((
            Mutex::new(QueueState {
                frames: VecDeque::new(),
                queued_bytes: 0,
                state: ProctoringState::Connecting,
                status_text: "Screen Proctoring (Window only): Connecting...".to_string(),
            }),
            Condvar::new(),
        ));
        let (log_tx, log_rx) = mpsc::channel();

        let w_running = Arc::clone(&running);
        let w_sent = Arc::clone(&sent_count);
        let w_shared = Arc::clone(&shared);
        let w_cfg = cfg.clone();

        let worker = thread::spawn(move || {
            worker_loop(base_url, w_cfg, w_running, w_sent, w_shared, log_tx);
        });

        Ok(Self {
            cfg,
            running,
            sent_count,
            shared,
            log_rx: Mutex::new(log_rx),
            worker: Mutex::new(Some(worker)),
        })
    }

    pub fn submit_window_rgba(
        &self,
        rgba: &[u8],
        width: u32,
        height: u32,
        meta: &MetadataInput,
    ) {
        if !self.running.load(Ordering::SeqCst) {
            return;
        }
        // Fallback to PNG if requested format encoding fails (matches C++ ScreenProctoringService)
        let encoded = process_and_encode_rgba(
            rgba,
            width,
            height,
            self.cfg.downscaling,
            self.cfg.quantization,
            self.cfg.format,
        )
        .or_else(|| {
            process_and_encode_rgba(
                rgba,
                width,
                height,
                self.cfg.downscaling,
                self.cfg.quantization,
                ImageFormatKind::Png,
            )
        });
        let Some(encoded) = encoded else { return };

        let metadata_json = build_metadata_json(meta);
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let frame = CaptureFrame {
            timestamp_ms: ts,
            image_format: encoded.format_name,
            image_data: encoded.bytes,
            metadata_json,
        };
        let frame_size = frame.image_data.len() + frame.metadata_json.len();
        let max_items = self.cfg.cache_size.max(10);

        let (lock, cvar) = &*self.shared;
        let mut q = lock.lock().unwrap();
        while !q.frames.is_empty()
            && (q.frames.len() >= max_items || q.queued_bytes + frame_size > MAX_QUEUED_BYTES)
        {
            if let Some(dropped) = q.frames.pop_front() {
                q.queued_bytes = q
                    .queued_bytes
                    .saturating_sub(dropped.image_data.len() + dropped.metadata_json.len());
            }
        }
        q.queued_bytes += frame_size;
        q.frames.push_back(frame);
        cvar.notify_one();
    }

    pub fn drain_logs(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        if let Ok(rx) = self.log_rx.lock() {
            while let Ok(item) = rx.try_recv() {
                out.push(item);
            }
        }
        out
    }

    pub fn snapshot(&self) -> (ProctoringState, String, usize, u64) {
        let q = self.shared.0.lock().unwrap();
        (
            q.state,
            q.status_text.clone(),
            q.frames.len(),
            self.sent_count.load(Ordering::Relaxed),
        )
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
        self.shared.1.notify_all();
        if let Ok(mut w) = self.worker.lock() {
            if let Some(handle) = w.take() {
                let _ = handle.join();
            }
        }
        if let Ok(mut q) = self.shared.0.lock() {
            q.frames.clear();
            q.queued_bytes = 0;
            q.state = ProctoringState::Inactive;
            q.status_text = "Screen Proctoring: Inactive".to_string();
        }
    }
}

impl Drop for ScreenProctoringClient {
    fn drop(&mut self) {
        self.stop();
    }
}

fn percent_encode_utf8(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len() * 2);
    for &b in input {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn generate_uuid_v4() -> String {
    let mut bytes = [0u8; 16];
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id() as u128;
    let seed = nanos ^ (pid << 64) ^ 0x9e3779b97f4a7c15u128;
    bytes.copy_from_slice(&seed.to_le_bytes());
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3],
        bytes[4], bytes[5],
        bytes[6], bytes[7],
        bytes[8], bytes[9],
        bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
    )
}

fn normalize_service_url(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let with_scheme = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };
    let mut url = Url::parse(&with_scheme).ok()?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return None;
    }
    url.set_query(None);
    url.set_fragment(None);
    let path = url.path().trim_end_matches('/').to_string();
    url.set_path(&path);
    Some(url.to_string().trim_end_matches('/').to_string())
}

fn set_state(shared: &(Mutex<QueueState>, Condvar), state: ProctoringState, text: &str) {
    if let Ok(mut q) = shared.0.lock() {
        q.state = state;
        q.status_text = text.to_string();
    }
}

fn worker_loop(
    base_url: String,
    cfg: ProctoringClientConfig,
    running: Arc<AtomicBool>,
    sent_count: Arc<AtomicU64>,
    shared: Arc<(Mutex<QueueState>, Condvar)>,
    log_tx: Sender<(String, String)>,
) {
    // Disable redirects to match C++ ManualRedirectPolicy!
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(15))
        .redirects(0)
        .build();
    let mut token = String::new();
    let mut session_id = cfg.session_id.trim().to_string();
    let mut session_opened = !session_id.is_empty();
    let mut backoff_ms: u64 = 2000;

    while running.load(Ordering::SeqCst) {
        // 1. OAuth2 if credentials configured
        if token.is_empty()
            && (!cfg.client_id.trim().is_empty() || !cfg.client_secret.trim().is_empty())
        {
            let creds = base64::engine::general_purpose::STANDARD
                .encode(format!("{}:{}", cfg.client_id, cfg.client_secret));
            match agent
                .post(&format!("{base_url}/oauth/token"))
                .set("Authorization", &format!("Basic {creds}"))
                .set("Content-Type", "application/x-www-form-urlencoded")
                .set("Accept", "application/json")
                .send_string("grant_type=client_credentials&scope=read%20write")
            {
                Ok(resp) => {
                    let mut body = Vec::new();
                    let _ = resp.into_reader().take(256 * 1024).read_to_end(&mut body);
                    if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&body) {
                        if let Some(t) = v.get("access_token").and_then(|x| x.as_str()) {
                            token = t.trim().to_string();
                        }
                    }
                    if token.is_empty() {
                        let msg = "Screen Proctoring: OAuth authentication failed (retrying)";
                        set_state(&shared, ProctoringState::Error, msg);
                        let _ = log_tx.send((msg.to_string(), "WARNING".to_string()));
                        sleep_interruptible(&running, &shared, backoff_ms);
                        backoff_ms = (backoff_ms * 2).min(30_000);
                        continue;
                    }
                }
                Err(_) => {
                    let msg = "Screen Proctoring: OAuth authentication failed (retrying)";
                    set_state(&shared, ProctoringState::Error, msg);
                    let _ = log_tx.send((msg.to_string(), "WARNING".to_string()));
                    sleep_interruptible(&running, &shared, backoff_ms);
                    backoff_ms = (backoff_ms * 2).min(30_000);
                    continue;
                }
            }
        }

        // 2. Open remote session if no session_id provided
        if !session_opened {
            let mut req = agent
                .post(&format!("{base_url}/seb-api/v1/session"))
                .set("Content-Type", "application/x-www-form-urlencoded")
                .set("Accept", "application/json, */*");
            if !token.is_empty() {
                req = req.set("Authorization", &format!("Bearer {token}"));
            }
            let body_str = if cfg.group_id.trim().is_empty() {
                String::new()
            } else {
                format!("groupId={}", percent_encode_utf8(cfg.group_id.trim().as_bytes()))
            };
            match req.send_string(&body_str) {
                Ok(resp) => {
                    let hdr_id = resp
                        .header("SEB_SPS_SESSION_ID")
                        .or_else(|| resp.header("SEB-Session-Id"))
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    let mut body = Vec::new();
                    let _ = resp.into_reader().take(256 * 1024).read_to_end(&mut body);
                    if !hdr_id.is_empty() {
                        session_id = hdr_id;
                    } else if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&body) {
                        session_id = v
                            .get("sessionId")
                            .or_else(|| v.get("uuid"))
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .trim()
                            .to_string();
                    }
                    if session_id.is_empty() {
                        session_id = generate_uuid_v4();
                    }
                    session_opened = true;
                    backoff_ms = 2000;
                    set_state(
                        &shared,
                        ProctoringState::Active,
                        "Screen Proctoring (Window only): Active",
                    );
                    let _ = log_tx.send((
                        "Screen Proctoring session opened.".to_string(),
                        "INFO".to_string(),
                    ));
                }
                Err(_) => {
                    let msg = "Screen Proctoring: Session handshake failed (retrying)";
                    set_state(&shared, ProctoringState::Error, msg);
                    let _ = log_tx.send((msg.to_string(), "WARNING".to_string()));
                    sleep_interruptible(&running, &shared, backoff_ms);
                    backoff_ms = (backoff_ms * 2).min(30_000);
                    continue;
                }
            }
        }

        // 3. Pop next frame or wait
        let (next_frame, remaining_queued) = {
            let (lock, cvar) = &*shared;
            let mut q = lock.lock().unwrap();
            while q.frames.is_empty() && running.load(Ordering::SeqCst) {
                let (guard, _) = cvar.wait_timeout(q, Duration::from_millis(500)).unwrap();
                q = guard;
            }
            if !running.load(Ordering::SeqCst) {
                (None, q.frames.len())
            } else {
                let f = q.frames.pop_front().map(|f| {
                    q.queued_bytes = q
                        .queued_bytes
                        .saturating_sub(f.image_data.len() + f.metadata_json.len());
                    f
                });
                (f, q.frames.len())
            }
        };

        let Some(frame) = next_frame else { continue };

        // 4. Upload frame to exact SEB SPS endpoint:
        //    POST {base_url}/seb-api/v1/session/{percentEncodedSessionId}/screenshot
        //    with ALL 6 headers (timestamp, SEB_SPS_TIMESTAMP, imageFormat, SEB_SPS_IMAGE_FORMAT,
        //    percent-encoded metaData & SEB_SPS_METADATA)!
        let encoded_session = percent_encode_utf8(session_id.as_bytes());
        let upload_url = format!("{base_url}/seb-api/v1/session/{encoded_session}/screenshot");
        let ts_str = frame.timestamp_ms.to_string();
        let encoded_meta = percent_encode_utf8(&frame.metadata_json);

        let mut req = agent
            .post(&upload_url)
            .set("Content-Type", "application/octet-stream")
            .set("Accept", "application/json, */*")
            .set("timestamp", &ts_str)
            .set("SEB_SPS_TIMESTAMP", &ts_str)
            .set("imageFormat", &frame.image_format)
            .set("SEB_SPS_IMAGE_FORMAT", &frame.image_format)
            .set("metaData", &encoded_meta)
            .set("SEB_SPS_METADATA", &encoded_meta);
        if !token.is_empty() {
            req = req.set("Authorization", &format!("Bearer {token}"));
        }

        match req.send_bytes(&frame.image_data) {
            Ok(_) => {
                let sent = sent_count.fetch_add(1, Ordering::Relaxed) + 1;
                backoff_ms = 2000;
                set_state(
                    &shared,
                    ProctoringState::Active,
                    &format!(
                        "Screen Proctoring (Window only): Active ({sent} sent, {remaining_queued} queued)"
                    ),
                );
            }
            Err(ureq::Error::Status(401, _)) => {
                token.clear();
                requeue_front(&shared, frame);
                sleep_interruptible(&running, &shared, 500);
            }
            Err(ureq::Error::Status(status, _)) => {
                requeue_front(&shared, frame);
                let queued = shared.0.lock().map(|q| q.frames.len()).unwrap_or(0);
                let msg = format!(
                    "Screen Proctoring: Upload error (HTTP {status}, {queued} queued — retrying)"
                );
                set_state(&shared, ProctoringState::Error, &msg);
                let _ = log_tx.send((msg, "WARNING".to_string()));
                sleep_interruptible(&running, &shared, backoff_ms);
                backoff_ms = (backoff_ms * 2).min(30_000);
            }
            Err(_) => {
                requeue_front(&shared, frame);
                let queued = shared.0.lock().map(|q| q.frames.len()).unwrap_or(0);
                let msg = format!(
                    "Screen Proctoring: Upload network error ({queued} queued — retrying)"
                );
                set_state(&shared, ProctoringState::Error, &msg);
                let _ = log_tx.send((msg, "WARNING".to_string()));
                sleep_interruptible(&running, &shared, backoff_ms);
                backoff_ms = (backoff_ms * 2).min(30_000);
            }
        }
    }

    // Close remote session on shutdown (only if session opened and authenticated)
    if session_opened && !session_id.is_empty() && !token.is_empty() {
        let encoded_session = percent_encode_utf8(session_id.as_bytes());
        let _ = agent
            .delete(&format!("{base_url}/seb-api/v1/session/{encoded_session}"))
            .set("Authorization", &format!("Bearer {token}"))
            .call();
    }
}

fn requeue_front(shared: &(Mutex<QueueState>, Condvar), frame: CaptureFrame) {
    if let Ok(mut q) = shared.0.lock() {
        q.queued_bytes += frame.image_data.len() + frame.metadata_json.len();
        q.frames.push_front(frame);
    }
}

fn sleep_interruptible(
    running: &AtomicBool,
    shared: &(Mutex<QueueState>, Condvar),
    ms: u64,
) {
    let (lock, cvar) = shared;
    let guard = lock.lock().unwrap();
    if running.load(Ordering::SeqCst) {
        let _ = cvar.wait_timeout(guard, Duration::from_millis(ms));
    }
}
