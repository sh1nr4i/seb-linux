use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use zbus::blocking::Connection;
use zbus::zvariant::OwnedFd;

/// Cross-distro power/sleep inhibitor:
/// 1. Pure-Rust D-Bus `org.freedesktop.login1.Manager.Inhibit("idle:sleep", ..., "block")`
///    (works seamlessly on both `systemd-logind` AND `elogind` on Void/Alpine/Artix/OpenRC).
/// 2. Session D-Bus `org.freedesktop.ScreenSaver` / `org.gnome.SessionManager` idle+suspend inhibit.
/// 3. Fallback to `systemd-inhibit` CLI if D-Bus is unreachable.
pub struct PowerInhibitor {
    child: Option<Child>,
    login_fd: Option<OwnedFd>,
    session_inhibits: Vec<(Connection, &'static str, &'static str, &'static str, &'static str, u32)>,
    required: bool,
}

impl PowerInhibitor {
    pub fn new() -> Self {
        Self {
            child: None,
            login_fd: None,
            session_inhibits: Vec::new(),
            required: false,
        }
    }

    pub fn start(&mut self, required: bool) -> Result<(), String> {
        self.stop();
        self.required = required;
        if !required {
            return Ok(());
        }

        // 1. System-level sleep+idle inhibitor (systemd-logind / elogind)
        if let Ok(sys) = Connection::system() {
            if let Ok(reply) = sys.call_method(
                Some("org.freedesktop.login1"),
                "/org/freedesktop/login1",
                Some("org.freedesktop.login1.Manager"),
                "Inhibit",
                &(
                    "idle:sleep",
                    "Safe Exam Browser",
                    "Exam session",
                    "block",
                ),
            ) {
                if let Ok(fd) = reply.body().deserialize::<OwnedFd>() {
                    self.login_fd = Some(fd);
                }
            }
        }

        // 2. Desktop session ScreenSaver / SessionManager inhibitor
        if let Ok(sess) = Connection::session() {
            for (dest, path, iface, uninhibit) in [
                (
                    "org.freedesktop.ScreenSaver",
                    "/org/freedesktop/ScreenSaver",
                    "org.freedesktop.ScreenSaver",
                    "UnInhibit",
                ),
                (
                    "org.freedesktop.ScreenSaver",
                    "/ScreenSaver",
                    "org.freedesktop.ScreenSaver",
                    "UnInhibit",
                ),
            ] {
                if let Ok(reply) = sess.call_method(
                    Some(dest),
                    path,
                    Some(iface),
                    "Inhibit",
                    &("Safe Exam Browser", "Exam session"),
                ) {
                    if let Ok(cookie) = reply.body().deserialize::<u32>() {
                        self.session_inhibits
                            .push((sess.clone(), dest, path, iface, uninhibit, cookie));
                        break;
                    }
                }
            }

            if let Ok(reply) = sess.call_method(
                Some("org.gnome.SessionManager"),
                "/org/gnome/SessionManager",
                Some("org.gnome.SessionManager"),
                "Inhibit",
                &("Safe Exam Browser", 0u32, "Exam session", 12u32),
            ) {
                if let Ok(cookie) = reply.body().deserialize::<u32>() {
                    self.session_inhibits.push((
                        sess,
                        "org.gnome.SessionManager",
                        "/org/gnome/SessionManager",
                        "org.gnome.SessionManager",
                        "Uninhibit",
                        cookie,
                    ));
                }
            }
        }

        if self.login_fd.is_some() || !self.session_inhibits.is_empty() {
            return Ok(());
        }

        // 3. CLI fallback if neither login1/elogind nor desktop ScreenSaver D-Bus responded
        let mut child = Command::new("systemd-inhibit")
            .args([
                "--what=idle:sleep",
                "--mode=block",
                "--who=Safe Exam Browser",
                "--why=Exam session",
                "sleep",
                "infinity",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| {
                "Unable to acquire power/sleep inhibitor via D-Bus (login1/ScreenSaver) or systemd-inhibit."
                    .to_string()
            })?;

        thread::sleep(Duration::from_millis(100));
        if let Ok(Some(_)) = child.try_wait() {
            return Err("systemd-inhibit exited immediately; cannot enforce always-on policy.".to_string());
        }
        self.child = Some(child);
        Ok(())
    }

    pub fn is_alive(&mut self) -> bool {
        if !self.required {
            return true;
        }
        if self.login_fd.is_some() || !self.session_inhibits.is_empty() {
            return true;
        }
        let Some(ref mut c) = self.child else {
            return false;
        };
        match c.try_wait() {
            Ok(None) => true,
            _ => {
                self.child = None;
                false
            }
        }
    }

    pub fn stop(&mut self) {
        self.required = false;
        self.login_fd.take();
        for (conn, dest, path, iface, method, cookie) in self.session_inhibits.drain(..) {
            let _ = conn.call_method(Some(dest), path, Some(iface), method, &(cookie,));
        }
        if let Some(mut c) = self.child.take() {
            if matches!(c.try_wait(), Ok(None)) {
                unsafe {
                    libc::kill(c.id() as libc::pid_t, libc::SIGTERM);
                }
                let deadline = Instant::now() + Duration::from_secs(1);
                loop {
                    if let Ok(Some(_)) = c.try_wait() {
                        break;
                    }
                    if Instant::now() >= deadline {
                        let _ = c.kill();
                        let _ = c.wait();
                        break;
                    }
                    thread::sleep(Duration::from_millis(25));
                }
            }
        }
    }
}

impl Default for PowerInhibitor {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for PowerInhibitor {
    fn drop(&mut self) {
        self.stop();
    }
}
