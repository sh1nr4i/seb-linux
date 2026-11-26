use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

pub const HEARTBEAT_INTERVAL_MS: u64 = 2000;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyboardLockPolicy {
    pub allow_alt_tab: bool,
    pub allow_alt_f4: bool,
    pub allow_alt_esc: bool,
    pub allow_ctrl_esc: bool,
    pub allow_print_screen: bool,
    pub allow_system_key: bool,
    pub allow_action_center_combo: bool,
}

impl KeyboardLockPolicy {
    /// Linux has no isolated Windows desktop, so OS-level shortcuts ALWAYS stay
    /// blocked at kernel level; only Action Center (`Super+A` -> `Ctrl+Shift+A`)
    /// follows the SEB config toggle.
    pub fn strict_linux(allow_action_center: bool) -> Self {
        Self {
            allow_alt_tab: false,
            allow_alt_f4: false,
            allow_alt_esc: false,
            allow_ctrl_esc: false,
            allow_print_screen: false,
            allow_system_key: false,
            allow_action_center_combo: allow_action_center,
        }
    }
}

pub fn default_prohibited_processes() -> Vec<String> {
    [
        "firefox",
        "firefox-bin",
        "chromium",
        "chrome",
        "google-chrome",
        "google-chrome-stable",
        "brave",
        "brave-browser",
        "vivaldi-bin",
        "opera",
        "zen-bin",
        "msedge",
        "microsoft-edge",
        "discord",
        "vesktop",
        "telegram-desktop",
        "slack",
        "teams",
        "teams-for-linux",
        "obs",
        "wf-recorder",
        "simplescreenrecorder",
        "kazam",
        "kooha",
        "gpu-screen-recorder",
        "gsr-ui",
        "vokoscreenng",
        "peek",
        "byzanz-record",
        "recordmydesktop",
        "spectacle",
        "flameshot",
        "gnome-screenshot",
        "ksnip",
        "scrot",
        "grim",
        "slurp",
        "anydesk",
        "teamviewer",
        "rustdesk",
        "vncviewer",
        "tigervnc",
        "wayvnc",
        "x11vnc",
        "remmina",
        "parsec",
        "sunshine",
        "moonlight",
        "virt-viewer",
        "gdb",
        "lldb",
        "strace",
        "ltrace",
        "frida-server",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect()
}

pub fn merge_prohibited_list(extra: &[String]) -> Vec<String> {
    let mut out = default_prohibited_processes();
    let mut seen: HashSet<String> = out.iter().map(|s| s.to_ascii_lowercase()).collect();
    for item in extra {
        let name = std::path::Path::new(item.trim())
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .trim();
        if !name.is_empty() && seen.insert(name.to_ascii_lowercase()) {
            out.push(name.to_string());
        }
    }
    out
}

#[derive(Debug, Clone)]
pub enum LockdownEvent {
    AltTabStep { backwards: bool },
    AltTabCommit,
    AltTabCancel,
    NavBack,
    NavForward,
    NavHome,
    CloseActiveWindow,
    Violation { reason: String, prohibited: Vec<String> },
    HelperTerminated { reason: String },
}

#[derive(Default)]
struct SyncState {
    ready: bool,
    locked: bool,
    keyboard_locked: bool,
    scan_seq: u64,
    last_scan: Vec<String>,
    exited: bool,
    last_error: String,
}

pub struct LockdownClient {
    child: Option<Child>,
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    sync: Arc<(Mutex<SyncState>, Condvar)>,
    stopping: Arc<AtomicBool>,
    event_tx: Sender<LockdownEvent>,
    event_rx: Mutex<Receiver<LockdownEvent>>,
}

impl LockdownClient {
    pub fn new() -> Self {
        let (event_tx, event_rx) = mpsc::channel();
        Self {
            child: None,
            stdin: Arc::new(Mutex::new(None)),
            sync: Arc::new((Mutex::new(SyncState::default()), Condvar::new())),
            stopping: Arc::new(AtomicBool::new(false)),
            event_tx,
            event_rx: Mutex::new(event_rx),
        }
    }

    pub fn poll_events(&self) -> Vec<LockdownEvent> {
        let mut out = Vec::new();
        if let Ok(rx) = self.event_rx.lock() {
            while let Ok(ev) = rx.try_recv() {
                out.push(ev);
            }
        }
        out
    }

    pub fn is_armed(&self) -> bool {
        self.sync.0.lock().map(|s| s.ready && !s.exited).unwrap_or(false)
    }

    pub fn is_locked(&self) -> bool {
        self.sync.0.lock().map(|s| s.locked && !s.exited).unwrap_or(false)
    }

    fn resolve_helper_path() -> Option<PathBuf> {
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                let sibling = dir.join("safe-exam-browser-lockdown");
                if sibling.is_file() {
                    return Some(sibling);
                }
            }
        }
        for candidate in [
            "/usr/libexec/safe-exam-browser-lockdown",
            "/usr/lib/safe-exam-browser/safe-exam-browser-lockdown",
            "/usr/bin/safe-exam-browser-lockdown",
        ] {
            let p = PathBuf::from(candidate);
            if p.is_file() {
                return Some(p);
            }
        }
        if let Ok(path_var) = std::env::var("PATH") {
            for dir in path_var.split(':') {
                let p = PathBuf::from(dir).join("safe-exam-browser-lockdown");
                if p.is_file() {
                    return Some(p);
                }
            }
        }
        None
    }

    fn find_escalator() -> Option<PathBuf> {
        // Primary: Polkit pkexec across standard desktop Linux distros (AGENTS.md)
        for candidate in ["/usr/bin/pkexec", "/bin/pkexec"] {
            let p = PathBuf::from(candidate);
            if p.is_file() {
                return Some(p);
            }
        }
        // Minimalist non-Polkit fallback (e.g. Alpine / Void / opendoas)
        for candidate in ["/usr/bin/doas", "/bin/doas"] {
            let p = PathBuf::from(candidate);
            if p.is_file() {
                return Some(p);
            }
        }
        None
    }

    pub fn start_helper(&mut self) -> Result<(), String> {
        if self.is_armed() {
            return Ok(());
        }
        self.stop();

        let escalator = Self::find_escalator().ok_or_else(|| {
            "Polkit (pkexec) or doas was not found. Administrator authentication is required for Secure Anti-Cheat Mode."
                .to_string()
        })?;
        let helper = Self::resolve_helper_path().ok_or_else(|| {
            "The Safe Exam Browser lockdown helper binary (safe-exam-browser-lockdown) was not found."
                .to_string()
        })?;

        // Allocate a fresh per-session stopping flag so any lingering thread from a
        // previous session can never clobber the new session's SyncState or send duplicate heartbeats.
        let session_stop = Arc::new(AtomicBool::new(false));
        self.stopping = Arc::clone(&session_stop);
        {
            let mut st = self.sync.0.lock().unwrap_or_else(|e| e.into_inner());
            *st = SyncState::default();
        }

        let mut child = Command::new(&escalator)
            .arg(&helper)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("Unable to start SEB lockdown helper: {e}"))?;

        let child_stdin = child.stdin.take();
        let child_stdout = child.stdout.take().ok_or("Failed to open helper stdout.")?;
        *self.stdin.lock().unwrap_or_else(|e| e.into_inner()) = child_stdin;
        self.child = Some(child);

        let sync = Arc::clone(&self.sync);
        let stopping = Arc::clone(&session_stop);
        let event_tx = self.event_tx.clone();
        let violation_reported = Arc::new(AtomicBool::new(false));

        thread::spawn(move || {
            let reader = BufReader::new(child_stdout);
            for line in reader.lines() {
                let Ok(raw) = line else { break };
                let trimmed = raw.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let Ok(val) = serde_json::from_str::<Value>(trimmed) else {
                    continue;
                };
                let ev = val.get("event").and_then(|v| v.as_str()).unwrap_or("");
                match ev {
                    "ready" => {
                        let (lock, cvar) = &*sync;
                        let mut st = lock.lock().unwrap();
                        st.ready = true;
                        cvar.notify_all();
                    }
                    "scan_result" => {
                        let list = parse_str_array(val.get("prohibited"));
                        let (lock, cvar) = &*sync;
                        let mut st = lock.lock().unwrap();
                        st.last_scan = list;
                        st.scan_seq += 1;
                        cvar.notify_all();
                    }
                    "locked" => {
                        let kb_locked = val
                            .get("keyboardLocked")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        let grabbed = val
                            .get("grabbedKeyboards")
                            .and_then(|v| v.as_i64())
                            .unwrap_or(0);
                        let grabbed_tp = val
                            .get("grabbedTouchpads")
                            .and_then(|v| v.as_i64())
                            .unwrap_or(0);
                        let warning = val
                            .get("warning")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .trim();
                        if !warning.is_empty() {
                            eprintln!("[Lockdown] {warning}");
                        } else {
                            eprintln!(
                                "[Lockdown] Kernel input filter active on {grabbed} keyboard(s) and {grabbed_tp} touchpad(s)."
                            );
                        }
                        let (lock, cvar) = &*sync;
                        let mut st = lock.lock().unwrap();
                        st.locked = true;
                        st.keyboard_locked = kb_locked;
                        cvar.notify_all();
                    }
                    "unlocked" => {
                        let (lock, cvar) = &*sync;
                        let mut st = lock.lock().unwrap();
                        st.locked = false;
                        st.keyboard_locked = false;
                        cvar.notify_all();
                    }
                    "violation" => {
                        if !stopping.load(Ordering::SeqCst)
                            && !violation_reported.swap(true, Ordering::SeqCst)
                        {
                            let reason = val
                                .get("reason")
                                .and_then(|v| v.as_str())
                                .unwrap_or("Prohibited application detected during exam.")
                                .to_string();
                            let prohibited = parse_str_array(val.get("prohibited"));
                            let _ = event_tx.send(LockdownEvent::Violation { reason, prohibited });
                        }
                    }
                    "pong" => {
                        let prohibited = parse_str_array(val.get("prohibited"));
                        if !prohibited.is_empty()
                            && !stopping.load(Ordering::SeqCst)
                            && !violation_reported.swap(true, Ordering::SeqCst)
                        {
                            let reason = format!(
                                "Prohibited applications detected during exam: {}",
                                prohibited.join(", ")
                            );
                            let _ = event_tx.send(LockdownEvent::Violation { reason, prohibited });
                        }
                    }
                    "alt_tab_step" => {
                        let backwards = val
                            .get("backwards")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        let _ = event_tx.send(LockdownEvent::AltTabStep { backwards });
                    }
                    "alt_tab_commit" => {
                        let _ = event_tx.send(LockdownEvent::AltTabCommit);
                    }
                    "alt_tab_cancel" => {
                        let _ = event_tx.send(LockdownEvent::AltTabCancel);
                    }
                    "nav_back" => {
                        let _ = event_tx.send(LockdownEvent::NavBack);
                    }
                    "nav_forward" => {
                        let _ = event_tx.send(LockdownEvent::NavForward);
                    }
                    "nav_home" => {
                        let _ = event_tx.send(LockdownEvent::NavHome);
                    }
                    "close_window" => {
                        let _ = event_tx.send(LockdownEvent::CloseActiveWindow);
                    }
                    "error" => {
                        let msg = val
                            .get("message")
                            .and_then(|v| v.as_str())
                            .unwrap_or("Lockdown helper error")
                            .to_string();
                        eprintln!("[Lockdown] {msg}");
                        let (lock, cvar) = &*sync;
                        let mut st = lock.lock().unwrap();
                        st.last_error = msg;
                        cvar.notify_all();
                    }
                    _ => {}
                }
            }

            let was_active = {
                let (lock, cvar) = &*sync;
                let mut st = lock.lock().unwrap();
                let active = st.ready || st.locked;
                st.ready = false;
                st.locked = false;
                st.exited = true;
                cvar.notify_all();
                active
            };
            if was_active && !stopping.load(Ordering::SeqCst) {
                let _ = event_tx.send(LockdownEvent::HelperTerminated {
                    reason: "The privileged SEB lockdown helper exited unexpectedly.".to_string(),
                });
            }
        });

        // Wait up to 120s for Polkit authentication + "ready"
        let deadline = Instant::now() + Duration::from_secs(120);
        let (lock, cvar) = &*self.sync;
        let mut st = lock.lock().unwrap();
        while !st.ready && !st.exited {
            let now = Instant::now();
            if now >= deadline {
                drop(st);
                self.stop();
                return Err("Administrator authentication timed out.".to_string());
            }
            let (guard, _) = cvar.wait_timeout(st, deadline - now).unwrap();
            st = guard;
        }
        if st.ready {
            return Ok(());
        }
        drop(st);

        let code = self
            .child
            .as_mut()
            .and_then(|c| c.wait().ok())
            .and_then(|s| s.code())
            .unwrap_or(-1);
        self.stop();
        if code == 126 || code == 127 {
            Err("Administrator authentication was cancelled or denied. Safe Exam Browser requires superuser lockdown in exam mode.".to_string())
        } else {
            Err(format!("Unable to start SEB lockdown helper (exit code {code})."))
        }
    }

    pub fn scan_prohibited_processes(&self, blacklist: &[String]) -> Result<Vec<String>, String> {
        if !self.is_armed() {
            return Err("SEB lockdown helper is not running.".to_string());
        }
        let target_seq = {
            let st = self.sync.0.lock().unwrap();
            st.scan_seq + 1
        };
        let pid = unsafe { libc::getpid() } as i64;
        self.send_json(&json!({
            "cmd": "scan_processes",
            "sebPid": pid,
            "blacklist": blacklist,
        }))?;

        let deadline = Instant::now() + Duration::from_secs(5);
        let (lock, cvar) = &*self.sync;
        let mut st = lock.lock().unwrap();
        while st.scan_seq < target_seq && !st.exited {
            let now = Instant::now();
            if now >= deadline {
                return Err("Timed out while scanning running processes.".to_string());
            }
            let (guard, _) = cvar.wait_timeout(st, deadline - now).unwrap();
            st = guard;
        }
        if st.scan_seq >= target_seq {
            Ok(st.last_scan.clone())
        } else {
            Err("SEB lockdown helper disconnected while scanning processes.".to_string())
        }
    }

    pub fn activate_lockdown(
        &mut self,
        policy: &KeyboardLockPolicy,
        blacklist: &[String],
    ) -> Result<(), String> {
        if !self.is_armed() {
            return Err("SEB lockdown helper is not running.".to_string());
        }
        if self.is_locked() {
            self.update_policy(policy, blacklist);
            return Ok(());
        }
        let pid = unsafe { libc::getpid() } as i64;
        self.send_json(&json!({
            "cmd": "lock",
            "sebPid": pid,
            "keyboard": policy,
            "blacklist": blacklist,
        }))?;

        let deadline = Instant::now() + Duration::from_secs(5);
        let (lock, cvar) = &*self.sync;
        let mut st = lock.lock().unwrap();
        while !st.locked && !st.exited {
            let now = Instant::now();
            if now >= deadline {
                return Err("Timed out while activating SEB lockdown.".to_string());
            }
            let (guard, _) = cvar.wait_timeout(st, deadline - now).unwrap();
            st = guard;
        }
        if !st.locked {
            return Err("Failed to activate SEB lockdown.".to_string());
        }
        if !st.keyboard_locked {
            drop(st);
            self.stop();
            return Err(
                "Kernel keyboard lockdown could not be activated (/dev/uinput or evdev unavailable)."
                    .to_string(),
            );
        }
        drop(st);

        // Start heartbeat watchdog feeder thread (2s interval)
        let stdin_arc = Arc::clone(&self.stdin);
        let sync_arc = Arc::clone(&self.sync);
        let stopping = Arc::clone(&self.stopping);
        thread::spawn(move || {
            let seb_pid = unsafe { libc::getpid() } as i64;
            while !stopping.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(HEARTBEAT_INTERVAL_MS));
                if stopping.load(Ordering::SeqCst) {
                    break;
                }
                let active = sync_arc
                    .0
                    .lock()
                    .map(|s| s.locked && !s.exited)
                    .unwrap_or(false);
                if !active {
                    break;
                }
                let line = format!("{{\"cmd\":\"ping\",\"sebPid\":{seb_pid}}}\n");
                if let Ok(mut guard) = stdin_arc.lock() {
                    if let Some(ref mut w) = *guard {
                        if w.write_all(line.as_bytes()).is_err() || w.flush().is_err() {
                            break;
                        }
                    } else {
                        break;
                    }
                }
            }
        });

        Ok(())
    }

    pub fn update_policy(&self, policy: &KeyboardLockPolicy, blacklist: &[String]) {
        if !self.is_armed() {
            return;
        }
        let pid = unsafe { libc::getpid() } as i64;
        let _ = self.send_json(&json!({
            "cmd": "update_policy",
            "sebPid": pid,
            "keyboard": policy,
            "blacklist": blacklist,
        }));
    }

    pub fn stop(&mut self) {
        self.stopping.store(true, Ordering::SeqCst);
        let _ = self.send_json(&json!({"cmd": "unlock"}));
        if let Ok(mut guard) = self.stdin.lock() {
            *guard = None; // Closes stdin pipe -> triggers fail-safe #2 EOF
        }
        if let Some(mut child) = self.child.take() {
            let deadline = Instant::now() + Duration::from_millis(1200);
            loop {
                if let Ok(Some(_)) = child.try_wait() {
                    break;
                }
                if Instant::now() >= deadline {
                    unsafe {
                        libc::kill(child.id() as libc::pid_t, libc::SIGTERM);
                    }
                    let term_deadline = Instant::now() + Duration::from_millis(800);
                    loop {
                        if let Ok(Some(_)) = child.try_wait() {
                            break;
                        }
                        if Instant::now() >= term_deadline {
                            // Note: pkexec is SUID-root so SIGKILL from unprivileged user may fail with EPERM.
                            // Never block indefinitely in child.wait() if kill() fails!
                            if child.kill().is_ok() {
                                let kill_deadline = Instant::now() + Duration::from_millis(500);
                                while Instant::now() < kill_deadline {
                                    if matches!(child.try_wait(), Ok(Some(_))) {
                                        break;
                                    }
                                    thread::sleep(Duration::from_millis(25));
                                }
                            }
                            break;
                        }
                        thread::sleep(Duration::from_millis(30));
                    }
                    break;
                }
                thread::sleep(Duration::from_millis(30));
            }
        }
        if let Ok(mut st) = self.sync.0.lock() {
            st.ready = false;
            st.locked = false;
            st.keyboard_locked = false;
            st.exited = true;
        }
    }

    fn send_json(&self, val: &Value) -> Result<(), String> {
        let mut line = serde_json::to_string(val).map_err(|e| e.to_string())?;
        line.push('\n');
        let mut guard = self.stdin.lock().unwrap();
        let writer = guard
            .as_mut()
            .ok_or_else(|| "Lockdown helper stdin is closed.".to_string())?;
        writer
            .write_all(line.as_bytes())
            .and_then(|_| writer.flush())
            .map_err(|e| format!("Failed writing to lockdown helper: {e}"))
    }
}

impl Default for LockdownClient {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for LockdownClient {
    fn drop(&mut self) {
        self.stop();
    }
}

fn parse_str_array(val: Option<&Value>) -> Vec<String> {
    val.and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}
