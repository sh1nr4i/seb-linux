use crate::{
    build_metadata_json, decrypt_cache_bytes, encrypt_cache_bytes, process_and_encode_rgba,
    ImageFormatKind, ImageQuantizationKind, MetadataInput,
};
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use url::Url;

const MAX_QUEUED_BYTES: usize = 128 * 1024 * 1024;
const IN_MEMORY_SOFT_LIMIT: usize = 4;

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
    pub encryption_secret: String,
    pub cache_dir: Option<PathBuf>,
    pub cache_size: usize,
    pub downscaling: f64,
    pub quantization: ImageQuantizationKind,
    pub format: ImageFormatKind,
}

#[derive(Serialize, Deserialize)]
struct CaptureFrame {
    timestamp_ms: u64,
    image_format: String,
    image_data: Vec<u8>,
    metadata_json: Vec<u8>,
}

enum QueuedItem {
    Memory(CaptureFrame),
    EncryptedDisk { path: PathBuf, approx_bytes: usize },
}

impl QueuedItem {
    fn byte_len(&self) -> usize {
        match self {
            QueuedItem::Memory(f) => f.image_data.len() + f.metadata_json.len(),
            QueuedItem::EncryptedDisk { approx_bytes, .. } => *approx_bytes,
        }
    }
}

struct QueueState {
    frames: VecDeque<QueuedItem>,
    queued_bytes: usize,
    next_disk_seq: u64,
    state: ProctoringState,
    status_text: String,
}

pub struct ScreenProctoringClient {
    cfg: ProctoringClientConfig,
    cache_dir: Option<PathBuf>,
    running: Arc<AtomicBool>,
    flushing: Arc<AtomicBool>,
    sent_count: Arc<AtomicU64>,
    shared: Arc<(Mutex<QueueState>, Condvar)>,
    log_rx: Mutex<Receiver<(String, String)>>,
    worker: Mutex<Option<thread::JoinHandle<()>>>,
}

impl ScreenProctoringClient {
    pub fn start(cfg: ProctoringClientConfig) -> Result<Self, String> {
        let base_url = normalize_service_url(&cfg.service_url)
            .ok_or_else(|| "Screen Proctoring could not start: invalid service URL.".to_string())?;
        let cache_dir = if !cfg.encryption_secret.trim().is_empty() {
            let dir = cfg.cache_dir.clone().unwrap_or_else(|| {
                std::env::temp_dir().join(format!(
                    "seb-sps-cache-{}-{}",
                    std::process::id(),
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|d| d.as_nanos())
                        .unwrap_or(0)
                ))
            });
            let _ = fs::create_dir_all(&dir);
            Some(dir)
        } else {
            None
        };
        let running = Arc::new(AtomicBool::new(true));
        let flushing = Arc::new(AtomicBool::new(false));
        let sent_count = Arc::new(AtomicU64::new(0));
        let shared = Arc::new((
            Mutex::new(QueueState {
                frames: VecDeque::new(),
                queued_bytes: 0,
                next_disk_seq: 1,
                state: ProctoringState::Connecting,
                status_text: "Screen Proctoring (Window only): Connecting...".to_string(),
            }),
            Condvar::new(),
        ));
        let (log_tx, log_rx) = mpsc::channel();

        let w_running = Arc::clone(&running);
        let w_flushing = Arc::clone(&flushing);
        let w_sent = Arc::clone(&sent_count);
        let w_shared = Arc::clone(&shared);
        let w_cfg = cfg.clone();

        let worker = thread::spawn(move || {
            worker_loop(
                base_url,
                w_cfg,
                w_running,
                w_flushing,
                w_sent,
                w_shared,
                log_tx,
            );
        });

        Ok(Self {
            cfg,
            cache_dir,
            running,
            flushing,
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
        if !self.running.load(Ordering::SeqCst) || self.flushing.load(Ordering::SeqCst) {
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
        let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
        while !q.frames.is_empty()
            && (q.frames.len() >= max_items || q.queued_bytes + frame_size > MAX_QUEUED_BYTES)
        {
            if let Some(dropped) = q.frames.pop_front() {
                q.queued_bytes = q.queued_bytes.saturating_sub(dropped.byte_len());
                if let QueuedItem::EncryptedDisk { path, .. } = dropped {
                    let _ = fs::remove_file(path);
                }
            }
        }

        // Spill to AES-GCM encrypted disk cache once in-memory soft limit is reached
        let item = if q.frames.len() >= IN_MEMORY_SOFT_LIMIT {
            if let Some(ref dir) = self.cache_dir {
                if let Ok(plain) = serde_json::to_vec(&frame) {
                    if let Some(enc) = encrypt_cache_bytes(&self.cfg.encryption_secret, &plain) {
                        let seq = q.next_disk_seq;
                        q.next_disk_seq += 1;
                        let path = dir.join(format!("frame-{seq:08}.bin"));
                        if fs::write(&path, &enc).is_ok() {
                            QueuedItem::EncryptedDisk {
                                path,
                                approx_bytes: enc.len(),
                            }
                        } else {
                            QueuedItem::Memory(frame)
                        }
                    } else {
                        QueuedItem::Memory(frame)
                    }
                } else {
                    QueuedItem::Memory(frame)
                }
            } else {
                QueuedItem::Memory(frame)
            }
        } else {
            QueuedItem::Memory(frame)
        };

        q.queued_bytes += item.byte_len();
        q.frames.push_back(item);
        cvar.notify_one();
    }

    pub fn pending_count(&self) -> usize {
        self.shared.0.lock().map(|q| q.frames.len()).unwrap_or(0)
    }

    /// Stops accepting new frames and wakes the worker to drain remaining queued frames.
    pub fn begin_flush(&self) {
        self.flushing.store(true, Ordering::SeqCst);
        self.shared.1.notify_all();
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
        let q = self.shared.0.lock().unwrap_or_else(|e| e.into_inner());
        (
            q.state,
            q.status_text.clone(),
            q.frames.len(),
            self.sent_count.load(Ordering::Relaxed),
        )
    }

    pub fn stop(&self) {
        self.flushing.store(true, Ordering::SeqCst);
        self.running.store(false, Ordering::SeqCst);
        self.shared.1.notify_all();
        if let Ok(mut w) = self.worker.lock() {
            if let Some(handle) = w.take() {
                let _ = handle.join();
            }
        }
        if let Ok(mut q) = self.shared.0.lock() {
            for item in q.frames.drain(..) {
                if let QueuedItem::EncryptedDisk { path, .. } = item {
                    let _ = fs::remove_file(path);
                }
            }
            q.queued_bytes = 0;
            q.state = ProctoringState::Inactive;
            q.status_text = "Screen Proctoring: Inactive".to_string();
        }
        if let Some(ref dir) = self.cache_dir {
            let _ = fs::remove_dir_all(dir);
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

fn query_sps_health(agent: &ureq::Agent, base_url: &str, token: &str) -> Option<u32> {
    let mut req = agent
        .get(&format!("{base_url}/seb-api/v1/health"))
        .set("Accept", "application/json, text/plain, */*");
    if !token.is_empty() {
        req = req.set("Authorization", &format!("Bearer {token}"));
    }
    let resp = req.call().ok()?;
    if let Some(hdr) = resp
        .header("SEB_SPS_HEALTH")
        .or_else(|| resp.header("seb-sps-health"))
    {
        if let Ok(v) = hdr.trim().parse::<u32>() {
            return Some(v.min(10));
        }
    }
    let mut body = Vec::new();
    let _ = resp.into_reader().take(4096).read_to_end(&mut body);
    if let Ok(text) = std::str::from_utf8(&body) {
        let t = text.trim();
        if let Ok(v) = t.parse::<u32>() {
            return Some(v.min(10));
        }
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(t) {
            if let Some(v) = json.get("health").and_then(|x| x.as_u64()) {
                return Some((v as u32).min(10));
            }
        }
    }
    None
}

fn worker_loop(
    base_url: String,
    cfg: ProctoringClientConfig,
    running: Arc<AtomicBool>,
    flushing: Arc<AtomicBool>,
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
    let mut last_health_check: Option<Instant> = None;
    let mut sps_health: u32 = 0;
    let mut consecutive_401: u32 = 0;

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
                Err(ureq::Error::Status(401, _)) => {
                    token.clear();
                    let msg = "Screen Proctoring: Session auth expired (retrying)";
                    set_state(&shared, ProctoringState::Error, msg);
                    let _ = log_tx.send((msg.to_string(), "WARNING".to_string()));
                    sleep_interruptible(&running, &shared, backoff_ms);
                    backoff_ms = (backoff_ms * 2).min(30_000);
                    continue;
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
        let (next_item, remaining_queued) = {
            let (lock, cvar) = &*shared;
            let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
            while q.frames.is_empty() && running.load(Ordering::SeqCst) {
                let (guard, _) = cvar
                    .wait_timeout(q, Duration::from_millis(500))
                    .unwrap_or_else(|e| e.into_inner());
                q = guard;
            }
            if !running.load(Ordering::SeqCst) {
                (None, q.frames.len())
            } else {
                let item = q.frames.pop_front().map(|it| {
                    q.queued_bytes = q.queued_bytes.saturating_sub(it.byte_len());
                    it
                });
                (item, q.frames.len())
            }
        };

        let Some(item) = next_item else { continue };
        let frame = match item {
            QueuedItem::Memory(f) => f,
            QueuedItem::EncryptedDisk { path, .. } => {
                let loaded = fs::read(&path)
                    .ok()
                    .and_then(|enc| decrypt_cache_bytes(&cfg.encryption_secret, &enc))
                    .and_then(|plain| serde_json::from_slice::<CaptureFrame>(&plain).ok());
                let _ = fs::remove_file(&path);
                let Some(f) = loaded else { continue };
                f
            }
        };

        // 3b. Periodic SPS server health check & adaptive spooling throttle (skipped during final flush)
        if !flushing.load(Ordering::SeqCst)
            && last_health_check.is_none_or(|t| t.elapsed() >= Duration::from_secs(10))
        {
            last_health_check = Some(Instant::now());
            if let Some(h) = query_sps_health(&agent, &base_url, &token) {
                sps_health = h;
            }
        }
        if !flushing.load(Ordering::SeqCst) && sps_health > 0 {
            let throttle_ms = (sps_health as u64 * 1000).min(10_000);
            sleep_interruptible(&running, &shared, throttle_ms);
            if !running.load(Ordering::SeqCst) {
                requeue_front(&shared, frame);
                break;
            }
        }

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
                consecutive_401 = 0;
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
                consecutive_401 = consecutive_401.saturating_add(1);
                let has_oauth =
                    !cfg.client_id.trim().is_empty() || !cfg.client_secret.trim().is_empty();
                if !has_oauth || consecutive_401 > 1 {
                    let msg = "Screen Proctoring: Upload unauthorized (HTTP 401 — retrying)";
                    set_state(&shared, ProctoringState::Error, msg);
                    let _ = log_tx.send((msg.to_string(), "WARNING".to_string()));
                    sleep_interruptible(&running, &shared, backoff_ms);
                    backoff_ms = (backoff_ms * 2).min(30_000);
                } else {
                    sleep_interruptible(&running, &shared, 500);
                }
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

    // Close remote session on shutdown (works with or without OAuth)
    if session_opened && !session_id.is_empty() {
        let encoded_session = percent_encode_utf8(session_id.as_bytes());
        let mut req = agent.delete(&format!("{base_url}/seb-api/v1/session/{encoded_session}"));
        if !token.is_empty() {
            req = req.set("Authorization", &format!("Bearer {token}"));
        }
        let _ = req.call();
    }
}

fn requeue_front(shared: &(Mutex<QueueState>, Condvar), frame: CaptureFrame) {
    if let Ok(mut q) = shared.0.lock() {
        let item = QueuedItem::Memory(frame);
        q.queued_bytes += item.byte_len();
        q.frames.push_front(item);
    }
}

fn sleep_interruptible(
    running: &AtomicBool,
    shared: &(Mutex<QueueState>, Condvar),
    ms: u64,
) {
    let deadline = Instant::now() + Duration::from_millis(ms);
    let (lock, cvar) = shared;
    let mut guard = lock.lock().unwrap_or_else(|e| e.into_inner());
    while running.load(Ordering::SeqCst) {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        let remaining = deadline.saturating_duration_since(now);
        let (next_guard, _) = cvar
            .wait_timeout(guard, remaining)
            .unwrap_or_else(|e| e.into_inner());
        guard = next_guard;
    }
}
