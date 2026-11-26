mod keyboard;
mod proc_scan;
mod protocol;
mod touchpad;

use keyboard::{FilterAction, KeyboardBlocker};
use proc_scan::scan_prohibited_processes;
use protocol::{IncomingCommand, KeyboardLockPolicy, HEARTBEAT_INTERVAL_MS, WATCHDOG_TIMEOUT_MS};
use touchpad::TouchpadBlocker;

use serde_json::json;
use std::io;
use std::os::fd::RawFd;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};

static SIGNAL_WRITE_FD: AtomicI32 = AtomicI32::new(-1);

extern "C" fn posix_signal_handler(_sig: libc::c_int) {
    let fd = SIGNAL_WRITE_FD.load(Ordering::Relaxed);
    if fd >= 0 {
        let byte: u8 = 1;
        unsafe {
            let _ = libc::write(fd, &byte as *const u8 as *const libc::c_void, 1);
        }
    }
}

fn write_json_line(value: &serde_json::Value) {
    let mut line = value.to_string();
    line.push('\n');
    let bytes = line.as_bytes();
    let mut offset = 0;
    while offset < bytes.len() {
        let written = unsafe {
            libc::write(
                libc::STDOUT_FILENO,
                bytes.as_ptr().add(offset) as *const libc::c_void,
                bytes.len() - offset,
            )
        };
        if written <= 0 {
            break;
        }
        offset += written as usize;
    }
}

fn main() {
    // Fail-safe #1: PR_SET_PDEATHSIG(SIGTERM) so kernel immediately terminates helper if parent dies.
    let parent_before = unsafe { libc::getppid() };
    unsafe {
        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
    }
    if unsafe { libc::getppid() } != parent_before {
        return;
    }

    let mut sig_fds = [-1i32; 2];
    if unsafe {
        libc::socketpair(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            0,
            sig_fds.as_mut_ptr(),
        )
    } == 0
    {
        SIGNAL_WRITE_FD.store(sig_fds[0], Ordering::Relaxed);
        unsafe {
            libc::signal(
                libc::SIGTERM,
                posix_signal_handler as *const () as libc::sighandler_t,
            );
            libc::signal(
                libc::SIGINT,
                posix_signal_handler as *const () as libc::sighandler_t,
            );
            libc::signal(
                libc::SIGHUP,
                posix_signal_handler as *const () as libc::sighandler_t,
            );
            libc::signal(libc::SIGPIPE, libc::SIG_IGN);
        }
    }

    // Make stdin non-blocking for poll() loop (Fail-safe #2: instant EOF detection).
    unsafe {
        let flags = libc::fcntl(libc::STDIN_FILENO, libc::F_GETFL, 0);
        if flags >= 0 {
            libc::fcntl(libc::STDIN_FILENO, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
    }

    let mut keyboard = KeyboardBlocker::new();
    let mut touchpad = TouchpadBlocker::new();
    let mut blacklist: Vec<String> = Vec::new();
    let mut seb_pid: i64 = 0;
    let mut locked = false;
    let mut last_activity = Instant::now();
    let mut last_monitor_scan = Instant::now();
    let mut input_buf: Vec<u8> = Vec::new();

    write_json_line(&json!({
        "event": "ready",
        "pid": unsafe { libc::getpid() as i64 },
        "euid": unsafe { libc::geteuid() as i64 },
    }));

    let mut running = true;
    while running {
        let tp_fds = touchpad.device_fds();
        let mut poll_fds: Vec<libc::pollfd> =
            Vec::with_capacity(2 + keyboard.grabbed_count() + tp_fds.len());
        poll_fds.push(libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        });
        if sig_fds[1] >= 0 {
            poll_fds.push(libc::pollfd {
                fd: sig_fds[1],
                events: libc::POLLIN,
                revents: 0,
            });
        }
        for &dev_fd in keyboard.device_fds() {
            poll_fds.push(libc::pollfd {
                fd: dev_fd,
                events: libc::POLLIN,
                revents: 0,
            });
        }
        for &tp_fd in &tp_fds {
            poll_fds.push(libc::pollfd {
                fd: tp_fd,
                events: libc::POLLIN,
                revents: 0,
            });
        }

        let timeout_ms: libc::c_int = if locked { 250 } else { 500 };
        let rc = unsafe {
            libc::poll(
                poll_fds.as_mut_ptr(),
                poll_fds.len() as libc::nfds_t,
                timeout_ms,
            )
        };
        if rc < 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            break;
        }

        // Check POSIX signal wake-up
        if sig_fds[1] >= 0 {
            if let Some(sig_pfd) = poll_fds.iter().find(|p| p.fd == sig_fds[1]) {
                if (sig_pfd.revents & libc::POLLIN) != 0 {
                    let mut drain = [0u8; 16];
                    unsafe {
                        let _ = libc::read(
                            sig_fds[1],
                            drain.as_mut_ptr() as *mut libc::c_void,
                            drain.len(),
                        );
                    }
                    break;
                }
            }
        }

        // Check keyboard evdev events first for zero-latency typing
        let kbd_start_idx = if sig_fds[1] >= 0 { 2 } else { 1 };
        let kbd_end_idx = kbd_start_idx + keyboard.grabbed_count();
        let ready_dev_fds: Vec<RawFd> = poll_fds[kbd_start_idx..kbd_end_idx]
            .iter()
            .filter(|p| (p.revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP)) != 0)
            .map(|p| p.fd)
            .collect();
        for dev_fd in ready_dev_fds {
            let actions = keyboard.handle_readable_fd(dev_fd);
            for act in actions {
                match act {
                    FilterAction::AltTabStep { backwards } => {
                        write_json_line(&json!({
                            "event": "alt_tab_step",
                            "backwards": backwards,
                        }));
                    }
                    FilterAction::AltTabCommit => {
                        write_json_line(&json!({ "event": "alt_tab_commit" }));
                    }
                    FilterAction::AltTabCancel => {
                        write_json_line(&json!({ "event": "alt_tab_cancel" }));
                    }
                    FilterAction::NavBack => {
                        write_json_line(&json!({ "event": "nav_back" }));
                    }
                    FilterAction::NavForward => {
                        write_json_line(&json!({ "event": "nav_forward" }));
                    }
                    FilterAction::NavHome => {
                        write_json_line(&json!({ "event": "nav_home" }));
                    }
                    FilterAction::CloseActiveWindow => {
                        write_json_line(&json!({ "event": "close_window" }));
                    }
                    _ => {}
                }
            }
        }

        // Filter touchpad multi-finger (>2 fingers) gestures at kernel evdev/uinput level
        let ready_tp_fds: Vec<RawFd> = poll_fds[kbd_end_idx..]
            .iter()
            .filter(|p| (p.revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP)) != 0)
            .map(|p| p.fd)
            .collect();
        for tp_fd in ready_tp_fds {
            touchpad.handle_readable_fd(tp_fd);
        }

        // Check stdin IPC commands / EOF
        let stdin_revents = poll_fds[0].revents;
        if (stdin_revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR)) != 0 {
            let mut chunk = [0u8; 4096];
            loop {
                let bytes = unsafe {
                    libc::read(
                        libc::STDIN_FILENO,
                        chunk.as_mut_ptr() as *mut libc::c_void,
                        chunk.len(),
                    )
                };
                if bytes > 0 {
                    input_buf.extend_from_slice(&chunk[..bytes as usize]);
                } else if bytes == 0 {
                    running = false;
                    break;
                } else {
                    let code = io::Error::last_os_error().raw_os_error().unwrap_or(0);
                    if code == libc::EAGAIN || code == libc::EWOULDBLOCK {
                        break;
                    }
                    running = false;
                    break;
                }
            }

            while let Some(nl) = input_buf.iter().position(|&b| b == b'\n') {
                let line_bytes: Vec<u8> = input_buf.drain(..=nl).collect();
                let line = String::from_utf8_lossy(&line_bytes).trim().to_string();
                if line.is_empty() {
                    continue;
                }
                let Ok(cmd) = serde_json::from_str::<IncomingCommand>(&line) else {
                    continue;
                };
                if let Some(pid) = cmd.seb_pid {
                    seb_pid = pid;
                }
                if locked {
                    last_activity = Instant::now();
                }

                match cmd.cmd.as_str() {
                    "ping" => {
                        let found = if locked {
                            scan_prohibited_processes(&blacklist, seb_pid)
                        } else {
                            Vec::new()
                        };
                        write_json_line(&json!({
                            "event": "pong",
                            "locked": locked,
                            "grabbedKeyboards": keyboard.grabbed_count(),
                            "grabbedTouchpads": touchpad.grabbed_count(),
                            "prohibited": found,
                        }));
                    }
                    "scan_processes" => {
                        let list = cmd.blacklist.unwrap_or_default();
                        let found = scan_prohibited_processes(&list, seb_pid);
                        write_json_line(&json!({
                            "event": "scan_result",
                            "prohibited": found,
                        }));
                    }
                    "lock" => {
                        blacklist = cmd.blacklist.unwrap_or_default();
                        let policy = cmd.keyboard.unwrap_or_else(KeyboardLockPolicy::default);
                        let (kb_locked, warning) = match keyboard.lock(policy) {
                            Ok(_) => (true, String::new()),
                            Err(e) => (false, e),
                        };
                        let _ = touchpad.lock();
                        locked = true;
                        last_activity = Instant::now();
                        last_monitor_scan = Instant::now();
                        write_json_line(&json!({
                            "event": "locked",
                            "keyboardLocked": kb_locked,
                            "grabbedKeyboards": keyboard.grabbed_count(),
                            "grabbedTouchpads": touchpad.grabbed_count(),
                            "warning": warning,
                        }));
                    }
                    "update_policy" => {
                        if let Some(list) = cmd.blacklist {
                            blacklist = list;
                        }
                        if let Some(policy) = cmd.keyboard {
                            keyboard.update_policy(policy);
                        }
                        write_json_line(&json!({ "event": "policy_updated" }));
                    }
                    "unlock" => {
                        running = false;
                        break;
                    }
                    _ => {}
                }
            }
        }

        // Fail-safe #3: Heartbeat watchdog (6s) + periodic prohibited process monitor (2s)
        if locked && running {
            if last_activity.elapsed() >= Duration::from_millis(WATCHDOG_TIMEOUT_MS) {
                touchpad.unlock();
                keyboard.unlock();
                locked = false;
                write_json_line(&json!({
                    "event": "error",
                    "message": "Watchdog timeout; released lockdown safely.",
                }));
                break;
            }
            if last_monitor_scan.elapsed() >= Duration::from_millis(HEARTBEAT_INTERVAL_MS) {
                last_monitor_scan = Instant::now();
                let found = scan_prohibited_processes(&blacklist, seb_pid);
                if !found.is_empty() {
                    write_json_line(&json!({
                        "event": "violation",
                        "reason": format!("Prohibited applications detected during exam: {}", found.join(", ")),
                        "prohibited": found,
                    }));
                }
            }
        }
    }

    if locked || keyboard.is_locked() || touchpad.is_locked() {
        touchpad.unlock();
        keyboard.unlock();
        write_json_line(&json!({ "event": "unlocked" }));
    }

    if sig_fds[0] >= 0 {
        unsafe { libc::close(sig_fds[0]) };
    }
    if sig_fds[1] >= 0 {
        unsafe { libc::close(sig_fds[1]) };
    }
}
