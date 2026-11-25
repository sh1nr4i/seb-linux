use seb_config::settings::WhitelistApplication;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use x11rb::connection::Connection as X11Connection;
use x11rb::protocol::xproto::{
    AtomEnum, ClientMessageEvent, ConnectionExt as X11ConnectionExt, EventMask, InputFocus,
    MapState, CLIENT_MESSAGE_EVENT,
};

#[derive(Debug, Clone)]
pub struct ExternalWindowInfo {
    pub x11_id: String,
    pub title: String,
}

struct RunningInstance {
    child: Child,
    pid: u32,
    windows: Vec<ExternalWindowInfo>,
}

pub struct ManagedExternalApp {
    pub config: WhitelistApplication,
    pub resolved_executable: PathBuf,
    instances: Vec<RunningInstance>,
}

impl ManagedExternalApp {
    pub fn display_title(&self) -> String {
        if !self.config.display_name.trim().is_empty() {
            self.config.display_name.clone()
        } else if !self.config.description.trim().is_empty() {
            self.config.description.clone()
        } else {
            self.config.executable_name.clone()
        }
    }

    pub fn tooltip(&self) -> String {
        if !self.config.description.trim().is_empty() {
            self.config.description.clone()
        } else {
            self.display_title()
        }
    }

    pub fn start(&mut self) {
        let expanded_args: Vec<String> = self
            .config
            .arguments
            .iter()
            .map(|arg| std::env::var(arg).unwrap_or_else(|_| arg.clone()))
            .collect();
        if let Ok(child) = Command::new(&self.resolved_executable)
            .args(&expanded_args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            let pid = child.id();
            self.instances.push(RunningInstance {
                child,
                pid,
                windows: Vec::new(),
            });
        }
    }

    pub fn poll_windows(&mut self) {
        let fallback_title = self.display_title();
        self.instances.retain_mut(|inst| {
            if let Ok(Some(_)) = inst.child.try_wait() {
                return false;
            }
            inst.windows = query_windows_for_pid(inst.pid, &fallback_title);
            true
        });
    }

    pub fn windows(&self) -> Vec<ExternalWindowInfo> {
        let mut out = Vec::new();
        for inst in &self.instances {
            if inst.windows.is_empty() {
                out.push(ExternalWindowInfo {
                    x11_id: String::new(),
                    title: self.display_title(),
                });
            } else {
                out.extend(inst.windows.clone());
            }
        }
        out
    }

    pub fn has_running_instances(&self) -> bool {
        !self.instances.is_empty()
    }

    pub fn activate_window(x11_id: &str) {
        let Ok(win) = x11_id.trim().parse::<u32>() else {
            return;
        };
        let Ok((conn, screen_num)) = x11rb::connect(None) else {
            return;
        };
        let Some(screen) = conn.setup().roots.get(screen_num) else {
            return;
        };
        let root = screen.root;
        if let Ok(cookie) = conn.intern_atom(false, b"_NET_ACTIVE_WINDOW") {
            if let Ok(reply) = cookie.reply() {
                let ev = ClientMessageEvent {
                    response_type: CLIENT_MESSAGE_EVENT,
                    format: 32,
                    sequence: 0,
                    window: win,
                    type_: reply.atom,
                    data: [2u32, 0, 0, 0, 0].into(),
                };
                let _ = conn.send_event(
                    false,
                    root,
                    EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
                    ev,
                );
            }
        }
        let _ = conn.set_input_focus(InputFocus::POINTER_ROOT, win, x11rb::CURRENT_TIME);
        let _ = conn.flush();
    }

    pub fn terminate_all(&mut self) {
        let force_kill = !self.config.allow_running || self.config.auto_terminate;
        for mut inst in self.instances.drain(..) {
            let sig = if force_kill {
                libc::SIGKILL
            } else {
                libc::SIGTERM
            };
            unsafe {
                libc::kill(inst.pid as libc::pid_t, sig);
            }
            let _ = inst.child.try_wait();
        }
    }
}

/// 100% parity with C++ `ApplicationManager` + `ApplicationFactory`:
/// Resolves whitelist executables, auto-starts `auto_start == true`, tracks running windows,
/// and terminates instances cleanly on session end/reconfiguration.
pub struct ApplicationManager {
    pub apps: Vec<ManagedExternalApp>,
}

impl ApplicationManager {
    pub fn initialize(whitelist: &[WhitelistApplication]) -> Self {
        let mut apps = Vec::new();
        for entry in whitelist {
            if let Some(resolved) = resolve_executable(&entry.executable_path, &entry.executable_name)
            {
                let mut managed = ManagedExternalApp {
                    config: entry.clone(),
                    resolved_executable: resolved,
                    instances: Vec::new(),
                };
                if entry.auto_start {
                    managed.start();
                }
                apps.push(managed);
            }
        }
        Self { apps }
    }

    pub fn poll_all(&mut self) {
        for app in &mut self.apps {
            app.poll_windows();
        }
    }

    pub fn terminate_all(&mut self) {
        for app in &mut self.apps {
            app.terminate_all();
        }
    }
}

impl Drop for ApplicationManager {
    fn drop(&mut self) {
        self.terminate_all();
    }
}

fn resolve_executable(path_str: &str, exe_name: &str) -> Option<PathBuf> {
    let trimmed_path = path_str.trim();
    let trimmed_exe = exe_name.trim();
    if !trimmed_path.is_empty() {
        let p = Path::new(trimmed_path);
        if p.is_file() {
            return Some(p.to_path_buf());
        }
        if p.is_dir() && !trimmed_exe.is_empty() {
            let joined = p.join(trimmed_exe);
            if joined.is_file() {
                return Some(joined);
            }
        }
    }
    if trimmed_exe.is_empty() {
        return None;
    }
    let direct = Path::new(trimmed_exe);
    if direct.is_file() {
        return Some(direct.to_path_buf());
    }
    if let Ok(path_env) = std::env::var("PATH") {
        for dir in path_env.split(':') {
            let cand = Path::new(dir).join(trimmed_exe);
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}

fn read_window_title_prop(
    conn: &impl X11ConnectionExt,
    wid: u32,
    prop: u32,
    type_atom: u32,
) -> Option<String> {
    let reply = conn
        .get_property(false, wid, prop, type_atom, 0, 256)
        .ok()?
        .reply()
        .ok()?;
    let s = String::from_utf8_lossy(&reply.value).trim().to_string();
    (!s.is_empty()).then_some(s)
}

fn query_windows_for_pid(pid: u32, fallback_title: &str) -> Vec<ExternalWindowInfo> {
    let Ok((conn, screen_num)) = x11rb::connect(None) else {
        return Vec::new();
    };
    let Some(screen) = conn.setup().roots.get(screen_num) else {
        return Vec::new();
    };
    let root = screen.root;
    let Some(atom_list) = conn
        .intern_atom(true, b"_NET_CLIENT_LIST")
        .ok()
        .and_then(|c| c.reply().ok())
    else {
        return Vec::new();
    };
    let Some(atom_pid) = conn
        .intern_atom(true, b"_NET_WM_PID")
        .ok()
        .and_then(|c| c.reply().ok())
    else {
        return Vec::new();
    };
    let atom_net_name = conn
        .intern_atom(true, b"_NET_WM_NAME")
        .ok()
        .and_then(|c| c.reply().ok())
        .map(|r| r.atom)
        .unwrap_or(0);
    let atom_utf8 = conn
        .intern_atom(true, b"UTF8_STRING")
        .ok()
        .and_then(|c| c.reply().ok())
        .map(|r| r.atom)
        .unwrap_or(0);

    let Some(clients_prop) = conn
        .get_property(false, root, atom_list.atom, AtomEnum::WINDOW, 0, 1024)
        .ok()
        .and_then(|c| c.reply().ok())
    else {
        return Vec::new();
    };
    let Some(wids) = clients_prop.value32() else {
        return Vec::new();
    };

    let mut windows = Vec::new();
    for wid in wids {
        let win_pid = conn
            .get_property(false, wid, atom_pid.atom, AtomEnum::CARDINAL, 0, 1)
            .ok()
            .and_then(|c| c.reply().ok())
            .and_then(|r| r.value32().and_then(|mut it| it.next()));
        if win_pid != Some(pid) {
            continue;
        }
        let viewable = conn
            .get_window_attributes(wid)
            .ok()
            .and_then(|c| c.reply().ok())
            .map(|attr| attr.map_state == MapState::VIEWABLE)
            .unwrap_or(false);
        if !viewable {
            continue;
        }
        let title = (atom_net_name != 0 && atom_utf8 != 0)
            .then(|| read_window_title_prop(&conn, wid, atom_net_name, atom_utf8))
            .flatten()
            .or_else(|| {
                read_window_title_prop(
                    &conn,
                    wid,
                    u32::from(AtomEnum::WM_NAME),
                    u32::from(AtomEnum::STRING),
                )
            })
            .unwrap_or_else(|| fallback_title.to_string());
        windows.push(ExternalWindowInfo {
            x11_id: wid.to_string(),
            title,
        });
    }
    windows
}
