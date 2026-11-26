use crate::protocol::{KeyboardLockPolicy, VIRTUAL_KEYBOARD_NAME};

use std::collections::HashSet;
use std::ffi::{CStr, CString};
use std::io;
use std::mem;
use std::os::fd::RawFd;
use std::time::{Duration, Instant};
use std::process::Command;
use std::thread;

// Linux input event constants (from <linux/input-event-codes.h> & <linux/uinput.h>)
pub const EV_SYN: u16 = 0x00;
pub const EV_KEY: u16 = 0x01;
pub const EV_REL: u16 = 0x02;
pub const EV_ABS: u16 = 0x03;
const EV_MAX: usize = 0x1f;
const REL_X: usize = 0x00;
const REL_Y: usize = 0x01;
const REL_MAX: usize = 0x0f;
const ABS_X: usize = 0x00;
const ABS_MT_POSITION_X: usize = 0x35;
const ABS_MAX: usize = 0x3f;
const KEY_MAX: usize = 0x2ff;
const SYN_REPORT: u16 = 0;

pub const KEY_ESC: u16 = 1;
#[allow(dead_code)]
pub const KEY_BACKSPACE: u16 = 14;
pub const KEY_TAB: u16 = 15;
pub const KEY_ENTER: u16 = 28;
pub const KEY_LEFTCTRL: u16 = 29;
pub const KEY_A: u16 = 30;
pub const KEY_GRAVE: u16 = 41;
pub const KEY_LEFTSHIFT: u16 = 42;
pub const KEY_RIGHTSHIFT: u16 = 54;
pub const KEY_LEFTALT: u16 = 56;
pub const KEY_SPACE: u16 = 57;
pub const KEY_F1: u16 = 59;
pub const KEY_F4: u16 = 62;
pub const KEY_F10: u16 = 68;
pub const KEY_F11: u16 = 87;
pub const KEY_F12: u16 = 88;
pub const KEY_RIGHTCTRL: u16 = 97;
pub const KEY_SYSRQ: u16 = 99;
pub const KEY_RIGHTALT: u16 = 100;
pub const KEY_HOME: u16 = 102;
pub const KEY_LEFT: u16 = 105;
pub const KEY_RIGHT: u16 = 106;
pub const KEY_POWER: u16 = 116;
pub const KEY_SCALE: u16 = 120;
pub const KEY_LEFTMETA: u16 = 125;
pub const KEY_RIGHTMETA: u16 = 126;
pub const KEY_COMPOSE: u16 = 127;
pub const KEY_OPEN: u16 = 134;
pub const KEY_FIND: u16 = 136;
pub const KEY_HELP: u16 = 138;
pub const KEY_MENU: u16 = 139;
pub const KEY_CALC: u16 = 140;
pub const KEY_SLEEP: u16 = 142;
pub const KEY_FILE: u16 = 144;
pub const KEY_PROG1: u16 = 148;
pub const KEY_PROG2: u16 = 149;
pub const KEY_WWW: u16 = 150;
pub const KEY_SCREENLOCK: u16 = 152;
pub const KEY_MAIL: u16 = 155;
pub const KEY_BOOKMARKS: u16 = 156;
pub const KEY_COMPUTER: u16 = 157;
pub const KEY_HOMEPAGE: u16 = 172;
pub const KEY_F13: u16 = 183;
pub const KEY_F24: u16 = 194;
pub const KEY_PROG3: u16 = 202;
pub const KEY_PROG4: u16 = 203;
pub const KEY_DASHBOARD: u16 = 204;
pub const KEY_SUSPEND: u16 = 205;
pub const KEY_PRINT: u16 = 210;
pub const KEY_CHAT: u16 = 216;
pub const KEY_SEARCH: u16 = 217;
pub const KEY_SWITCHVIDEOMODE: u16 = 227;
const BUS_USB: u16 = 0x03;

// Linux ioctl numbers (x86_64/aarch64 standard _IO/_IOW/_IOR)
const EVIOCGRAB: libc::c_ulong = 0x4004_4590;
const UI_DEV_CREATE: libc::c_ulong = 0x5501;
const UI_DEV_DESTROY: libc::c_ulong = 0x5502;
const UI_SET_EVBIT: libc::c_ulong = 0x4004_5564;
const UI_SET_KEYBIT: libc::c_ulong = 0x4004_5565;
const UI_DEV_SETUP: libc::c_ulong = 0x405c_5503;

pub const fn ior(ty: u32, nr: u32, size: usize) -> libc::c_ulong {
    ((2u32 << 30) | ((size as u32) << 16) | (ty << 8) | nr) as libc::c_ulong
}

pub const fn eviocgname(len: usize) -> libc::c_ulong {
    ior(b'E' as u32, 0x06, len)
}

pub const fn eviocgbit(ev: u32, len: usize) -> libc::c_ulong {
    ior(b'E' as u32, 0x20 + ev, len)
}

pub const fn eviocgkey(len: usize) -> libc::c_ulong {
    ior(b'E' as u32, 0x18, len)
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct InputEvent {
    pub time: libc::timeval,
    pub type_: u16,
    pub code: u16,
    pub value: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct InputId {
    bustype: u16,
    vendor: u16,
    product: u16,
    version: u16,
}

#[repr(C)]
struct UinputSetup {
    id: InputId,
    name: [libc::c_char; 80],
    ff_effects_max: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilterAction {
    Forward,
    Drop,
    EmitActionCenter,
    AltTabStep { backwards: bool },
    AltTabCommit,
    AltTabCancel,
    NavBack,
    NavForward,
    NavHome,
    CloseActiveWindow,
}

#[derive(Debug, Default)]
pub struct KeyFilterState {
    pub policy: KeyboardLockPolicy,
    suppressed_keys: HashSet<(RawFd, u16)>,
    forwarded_keys: HashSet<(RawFd, u16)>,
    held_left_ctrl: HashSet<RawFd>,
    held_right_ctrl: HashSet<RawFd>,
    held_left_alt: HashSet<RawFd>,
    held_right_alt: HashSet<RawFd>,
    held_left_shift: HashSet<RawFd>,
    held_right_shift: HashSet<RawFd>,
    held_left_meta: HashSet<RawFd>,
    held_right_meta: HashSet<RawFd>,
    alt_tab_active: bool,
    alt_tab_repeating: bool,
    last_alt_tab_step: Option<Instant>,
}

impl KeyFilterState {
    pub fn reset(&mut self) {
        self.suppressed_keys.clear();
        self.forwarded_keys.clear();
        self.held_left_ctrl.clear();
        self.held_right_ctrl.clear();
        self.held_left_alt.clear();
        self.held_right_alt.clear();
        self.held_left_shift.clear();
        self.held_right_shift.clear();
        self.held_left_meta.clear();
        self.held_right_meta.clear();
        self.alt_tab_active = false;
        self.alt_tab_repeating = false;
        self.last_alt_tab_step = None;
    }

    pub fn ctrl_held(&self) -> bool {
        !self.held_left_ctrl.is_empty() || !self.held_right_ctrl.is_empty()
    }

    pub fn shift_held(&self) -> bool {
        !self.held_left_shift.is_empty() || !self.held_right_shift.is_empty()
    }

    fn alt_held(&self) -> bool {
        !self.held_left_alt.is_empty() || !self.held_right_alt.is_empty()
    }

    fn meta_held(&self) -> bool {
        !self.held_left_meta.is_empty() || !self.held_right_meta.is_empty()
    }

    /// Synchronize any keys already physically held when `EVIOCGRAB` takes over a device,
    /// so an in-flight Alt/Ctrl/Meta press never causes `Tab` or orphan key-ups to leak.
    pub fn sync_device_held_keys(&mut self, device_id: RawFd, key_state: &[u8]) {
        let mut update = |set: &mut HashSet<RawFd>, code: u16| {
            if test_bit(key_state, code as usize) {
                set.insert(device_id);
                self.suppressed_keys.insert((device_id, code));
            }
        };
        update(&mut self.held_left_ctrl, KEY_LEFTCTRL);
        update(&mut self.held_right_ctrl, KEY_RIGHTCTRL);
        update(&mut self.held_left_alt, KEY_LEFTALT);
        update(&mut self.held_right_alt, KEY_RIGHTALT);
        update(&mut self.held_left_shift, KEY_LEFTSHIFT);
        update(&mut self.held_right_shift, KEY_RIGHTSHIFT);
        update(&mut self.held_left_meta, KEY_LEFTMETA);
        update(&mut self.held_right_meta, KEY_RIGHTMETA);
        for code in 1..=248u16 {
            if test_bit(key_state, code as usize) {
                self.suppressed_keys.insert((device_id, code));
            }
        }
    }

    /// Cleanly release modifier tracking if a grabbed keyboard is hot-unplugged (`ENODEV` / EOF).
    /// Returns `(released_keycodes_for_uinput, optional_alt_tab_commit)`.
    pub fn clear_device(&mut self, device_id: RawFd) -> (Vec<u16>, Option<FilterAction>) {
        self.suppressed_keys.retain(|&(d, _)| d != device_id);
        let mut dev_keys: Vec<u16> = self
            .forwarded_keys
            .iter()
            .filter(|&&(d, _)| d == device_id)
            .map(|&(_, c)| c)
            .collect();
        self.forwarded_keys.retain(|&(d, _)| d != device_id);
        // Only emit release on /dev/uinput for keys not still held on another keyboard
        dev_keys.retain(|&c| !self.forwarded_keys.iter().any(|&(_, other)| other == c));
        self.held_left_ctrl.remove(&device_id);
        self.held_right_ctrl.remove(&device_id);
        self.held_left_alt.remove(&device_id);
        self.held_right_alt.remove(&device_id);
        self.held_left_shift.remove(&device_id);
        self.held_right_shift.remove(&device_id);
        self.held_left_meta.remove(&device_id);
        self.held_right_meta.remove(&device_id);
        let commit = if self.alt_tab_active && !self.alt_held() {
            self.alt_tab_active = false;
            self.alt_tab_repeating = false;
            self.last_alt_tab_step = None;
            Some(FilterAction::AltTabCommit)
        } else {
            None
        };
        (dev_keys, commit)
    }

    #[cfg(test)]
    pub fn process_key(&mut self, code: u16, value: i32) -> (Option<FilterAction>, FilterAction) {
        self.process_key_for_device(0, code, value)
    }

    pub fn process_key_for_device(
        &mut self,
        device_id: RawFd,
        code: u16,
        value: i32,
    ) -> (Option<FilterAction>, FilterAction) {
        let pressed = value != 0;
        let set_mod = |set: &mut HashSet<RawFd>, dev: RawFd, down: bool| {
            if down {
                set.insert(dev);
            } else {
                set.remove(&dev);
            }
        };
        match code {
            KEY_LEFTCTRL => set_mod(&mut self.held_left_ctrl, device_id, pressed),
            KEY_RIGHTCTRL => set_mod(&mut self.held_right_ctrl, device_id, pressed),
            KEY_LEFTALT => set_mod(&mut self.held_left_alt, device_id, pressed),
            KEY_RIGHTALT => set_mod(&mut self.held_right_alt, device_id, pressed),
            KEY_LEFTSHIFT => set_mod(&mut self.held_left_shift, device_id, pressed),
            KEY_RIGHTSHIFT => set_mod(&mut self.held_right_shift, device_id, pressed),
            KEY_LEFTMETA => set_mod(&mut self.held_left_meta, device_id, pressed),
            KEY_RIGHTMETA => set_mod(&mut self.held_right_meta, device_id, pressed),
            _ => {}
        }

        let mut side_event = None;
        if self.alt_tab_active
            && (code == KEY_LEFTALT || code == KEY_RIGHTALT)
            && value == 0
            && !self.alt_held()
        {
            self.alt_tab_active = false;
        self.alt_tab_repeating = false;
        self.last_alt_tab_step = None;
            side_event = Some(FilterAction::AltTabCommit);
        }

        // CRITICAL INVARIANT: Any key previously forwarded as pressed to /dev/uinput MUST
        // always have its release forwarded, even if Alt/Ctrl/Meta was pressed in between
        // (e.g. hold TAB -> press ALT -> release TAB), otherwise the key stays stuck repeating!
        if value == 0 && self.forwarded_keys.remove(&(device_id, code)) {
            self.suppressed_keys.remove(&(device_id, code));
            return (side_event, FilterAction::Forward);
        }

        let ctrl = self.ctrl_held();
        let alt = self.alt_held();
        let meta = self.meta_held();
        let left_alt = !self.held_left_alt.is_empty();
        let right_alt = !self.held_right_alt.is_empty();

        if !self.policy.allow_system_key && (code == KEY_LEFTMETA || code == KEY_RIGHTMETA) {
            return (side_event, FilterAction::Drop);
        }

        // CRITICAL: Never forward LeftAlt to /dev/uinput when OS Alt shortcuts are blocked.
        // Keeping LeftAlt completely invisible to the OS compositor eliminates both:
        // 1) OS Alt+Tab race conditions during rapid Alt+Tab spamming, and
        // 2) KWin/GNOME Alt+Touchpad (drag/click/scroll) window-escape shortcuts.
        // SEB internal Alt+Tab and Alt+Left/Right/Home continue working via IPC.
        let suppress_left_alt = !self.policy.allow_alt_tab
            && !self.policy.allow_alt_f4
            && !self.policy.allow_alt_esc;
        if suppress_left_alt && code == KEY_LEFTALT {
            return (side_event, FilterAction::Drop);
        }

        if value == 0 && self.suppressed_keys.remove(&(device_id, code)) {
            return (side_event, FilterAction::Drop);
        }

        if self.alt_tab_active && code == KEY_ESC {
            if value == 1 {
                self.suppressed_keys.insert((device_id, code));
                self.alt_tab_active = false;
        self.alt_tab_repeating = false;
        self.last_alt_tab_step = None;
                return (side_event, FilterAction::AltTabCancel);
            }
            return (side_event, FilterAction::Drop);
        }

        if !self.policy.allow_alt_tab && (alt || self.alt_tab_active) && !ctrl && !meta && code == KEY_TAB {
            if value == 1 || value == 2 {
                self.suppressed_keys.insert((device_id, code));
                self.alt_tab_active = true;
                let now = Instant::now();
                let min_gap = if value == 2 {
                    // Require a deliberate 550ms initial hold before auto-repeat begins,
                    // then step every 250ms while held.
                    if self.alt_tab_repeating {
                        Duration::from_millis(250)
                    } else {
                        Duration::from_millis(550)
                    }
                } else {
                    // Short 45ms anti-bounce on fresh press
                    Duration::from_millis(45)
                };
                if let Some(prev) = self.last_alt_tab_step {
                    if now.duration_since(prev) < min_gap {
                        return (side_event, FilterAction::Drop);
                    }
                }
                self.alt_tab_repeating = value == 2;
                self.last_alt_tab_step = Some(now);
                return (
                    side_event,
                    FilterAction::AltTabStep {
                        backwards: self.shift_held(),
                    },
                );
            }
            return (side_event, FilterAction::Drop);
        }

        // Translate Alt+F4 via IPC so SEB closes its own active internal window (or requests quit
        // with Quit Password) without ever forwarding Alt+F4 to the OS compositor!
        if alt && !ctrl && !meta && code == (KEY_F1 + 3) {
            if value == 1 {
                self.suppressed_keys.insert((device_id, code));
                return (side_event, FilterAction::CloseActiveWindow);
            }
            return (side_event, FilterAction::Drop);
        }

        // Translate Alt+Left / Alt+Right / Alt+Home via IPC so browser navigation still works
        // without ever emitting KEY_LEFTALT to the OS compositor.
        if alt && !ctrl && !meta && matches!(code, KEY_LEFT | KEY_RIGHT | KEY_HOME) {
            if value == 1 {
                self.suppressed_keys.insert((device_id, code));
                let nav = match code {
                    KEY_LEFT => FilterAction::NavBack,
                    KEY_RIGHT => FilterAction::NavForward,
                    _ => FilterAction::NavHome,
                };
                return (side_event, nav);
            }
            return (side_event, FilterAction::Drop);
        }

        if !self.policy.allow_system_key && meta && code == KEY_A {
            if value == 1 {
                self.suppressed_keys.insert((device_id, code));
                if self.policy.allow_action_center_combo {
                    return (side_event, FilterAction::EmitActionCenter);
                }
            }
            return (side_event, FilterAction::Drop);
        }

        if !self.policy.allow_system_key && meta {
            if value != 0 {
                self.suppressed_keys.insert((device_id, code));
            }
            return (side_event, FilterAction::Drop);
        }

        if ctrl
            && alt
            && !matches!(
                code,
                KEY_LEFTCTRL
                    | KEY_RIGHTCTRL
                    | KEY_LEFTALT
                    | KEY_RIGHTALT
                    | KEY_LEFTSHIFT
                    | KEY_RIGHTSHIFT
            )
        {
            if value != 0 {
                self.suppressed_keys.insert((device_id, code));
            }
            return (side_event, FilterAction::Drop);
        }

        if alt
            && !matches!(
                code,
                KEY_LEFTALT
                    | KEY_RIGHTALT
                    | KEY_LEFTSHIFT
                    | KEY_RIGHTSHIFT
                    | KEY_LEFTCTRL
                    | KEY_RIGHTCTRL
            )
        {
            let allowed_alt = (self.policy.allow_alt_tab && code == KEY_TAB)
                || (self.policy.allow_alt_f4 && code == KEY_F4)
                || (self.policy.allow_alt_esc && code == KEY_ESC)
                || (!left_alt
                    && right_alt
                    && !is_function_key(code)
                    && !matches!(code, KEY_TAB | KEY_SPACE | KEY_ESC | KEY_GRAVE))
                || matches!(code, KEY_LEFT | KEY_RIGHT | KEY_HOME);
            if !allowed_alt {
                if value != 0 {
                    self.suppressed_keys.insert((device_id, code));
                }
                return (side_event, FilterAction::Drop);
            }
        }

        if ctrl && (is_function_key(code) || (!self.policy.allow_ctrl_esc && code == KEY_ESC)) {
            if value != 0 {
                self.suppressed_keys.insert((device_id, code));
            }
            return (side_event, FilterAction::Drop);
        }

        let blocked = (!self.policy.allow_print_screen && matches!(code, KEY_SYSRQ | KEY_PRINT))
            || (!self.policy.allow_system_key
                && (is_system_launcher_key(code) || (KEY_F13..=KEY_F24).contains(&code)));

        if blocked {
            if value != 0 {
                self.suppressed_keys.insert((device_id, code));
            }
            return (side_event, FilterAction::Drop);
        }

        if value != 0 {
            self.forwarded_keys.insert((device_id, code));
        } else {
            self.forwarded_keys.remove(&(device_id, code));
        }
        (side_event, FilterAction::Forward)
    }
}

fn is_function_key(code: u16) -> bool {
    (KEY_F1..=KEY_F10).contains(&code)
        || code == KEY_F11
        || code == KEY_F12
        || (KEY_F13..=KEY_F24).contains(&code)
}

fn is_system_launcher_key(code: u16) -> bool {
    matches!(
        code,
        KEY_MENU
            | KEY_COMPOSE
            | KEY_SLEEP
            | KEY_POWER
            | KEY_SUSPEND
            | KEY_SCREENLOCK
            | KEY_SEARCH
            | KEY_HOMEPAGE
            | KEY_CALC
            | KEY_FILE
            | KEY_WWW
            | KEY_DASHBOARD
            | KEY_SCALE
            | KEY_SWITCHVIDEOMODE
            | KEY_PROG1
            | KEY_PROG2
            | KEY_PROG3
            | KEY_PROG4
            | KEY_BOOKMARKS
            | KEY_COMPUTER
            | KEY_MAIL
            | KEY_CHAT
            | KEY_HELP
            | KEY_OPEN
            | KEY_FIND
    )
}

pub fn test_bit(bits: &[u8], bit: usize) -> bool {
    let idx = bit / 8;
    idx < bits.len() && (bits[idx] & (1u8 << (bit % 8))) != 0
}

pub struct KeyboardBlocker {
    uinput_fd: RawFd,
    device_fds: Vec<RawFd>,
    locked: bool,
    right_alt_forwarded: bool,
    filter: KeyFilterState,
}

impl KeyboardBlocker {
    pub fn new() -> Self {
        Self {
            uinput_fd: -1,
            device_fds: Vec::new(),
            locked: false,
            right_alt_forwarded: false,
            filter: KeyFilterState::default(),
        }
    }

    pub fn is_locked(&self) -> bool {
        self.locked
    }

    pub fn grabbed_count(&self) -> usize {
        self.device_fds.len()
    }

    pub fn device_fds(&self) -> &[RawFd] {
        &self.device_fds
    }

    pub fn update_policy(&mut self, policy: KeyboardLockPolicy) {
        self.filter.policy = policy;
    }

    pub fn lock(&mut self, policy: KeyboardLockPolicy) -> Result<usize, String> {
        self.filter.policy = policy;
        if self.locked {
            return Ok(self.device_fds.len());
        }

        self.filter.reset();
        self.right_alt_forwarded = false;
        self.create_virtual_keyboard()?;
        thread::sleep(Duration::from_millis(80));

        let dir_path = CString::new("/dev/input").unwrap();
        let dir = unsafe { libc::opendir(dir_path.as_ptr()) };
        if dir.is_null() {
            let err = io::Error::last_os_error();
            self.destroy_virtual_keyboard();
            return Err(format!("Unable to open /dev/input: {err}"));
        }

        loop {
            let ent = unsafe { libc::readdir(dir) };
            if ent.is_null() {
                break;
            }
            let name = unsafe { CStr::from_ptr((*ent).d_name.as_ptr()) };
            let Ok(name_str) = name.to_str() else {
                continue;
            };
            if !name_str.starts_with("event") {
                continue;
            }
            let full_path = format!("/dev/input/{name_str}");
            let Ok(c_path) = CString::new(full_path) else {
                continue;
            };
            let fd = unsafe {
                libc::open(
                    c_path.as_ptr(),
                    libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                continue;
            }
            if !Self::is_physical_keyboard(fd, name_str) {
                unsafe { libc::close(fd) };
                continue;
            }
            // Wait briefly for any physically held keys (e.g. Enter from Polkit prompt or Alt)
            // to release before EVIOCGRAB so libinput does not see stuck keys on the physical node.
            Self::wait_keys_released(fd, Duration::from_millis(300));
            if unsafe { libc::ioctl(fd, EVIOCGRAB, 1 as libc::c_int) } < 0 {
                unsafe { libc::close(fd) };
                continue;
            }
            // Drain any stale pre-grab events buffered during wait_keys_released before reading EVIOCGKEY
            let mut drain_buf = [InputEvent::default(); 64];
            let drain_bytes = drain_buf.len() * mem::size_of::<InputEvent>();
            loop {
                let n = unsafe {
                    libc::read(
                        fd,
                        drain_buf.as_mut_ptr() as *mut libc::c_void,
                        drain_bytes,
                    )
                };
                if n <= 0 {
                    break;
                }
            }
            let mut key_state = [0u8; (KEY_MAX / 8) + 1];
            if unsafe {
                libc::ioctl(
                    fd,
                    eviocgkey(key_state.len()),
                    key_state.as_mut_ptr() as *mut libc::c_void,
                )
            } >= 0
            {
                self.filter.sync_device_held_keys(fd, &key_state);
            }
            self.device_fds.push(fd);
        }
        unsafe { libc::closedir(dir) };

        if self.device_fds.is_empty() {
            self.destroy_virtual_keyboard();
            return Err(
                "No physical keyboard input devices could be locked via evdev.".to_string(),
            );
        }

        self.locked = true;
        Ok(self.device_fds.len())
    }

    pub fn unlock(&mut self) {
        self.locked = false;
        for fd in self.device_fds.drain(..) {
            unsafe {
                libc::ioctl(fd, EVIOCGRAB, 0 as libc::c_int);
                libc::close(fd);
            }
        }
        self.destroy_virtual_keyboard();
        self.right_alt_forwarded = false;
        self.filter.reset();
    }

    pub fn handle_readable_fd(&mut self, fd: RawFd) -> Vec<FilterAction> {
        let mut emitted_ipc = Vec::new();
        let mut events = [InputEvent::default(); 32];
        let event_size = mem::size_of::<InputEvent>();

        loop {
            let bytes = unsafe {
                libc::read(
                    fd,
                    events.as_mut_ptr() as *mut libc::c_void,
                    events.len() * event_size,
                )
            };
            if bytes < 0 {
                let err = io::Error::last_os_error();
                let code = err.raw_os_error().unwrap_or(0);
                if code == libc::EAGAIN || code == libc::EWOULDBLOCK || code == libc::EINTR {
                    break;
                }
                self.detach_dead_fd(fd, &mut emitted_ipc);
                break;
            }
            if bytes == 0 {
                self.detach_dead_fd(fd, &mut emitted_ipc);
                break;
            }

            let count = (bytes as usize) / event_size;
            for ev in &events[..count] {
                self.process_input_event(fd, ev, &mut emitted_ipc);
            }
        }

        emitted_ipc
    }

    fn detach_dead_fd(&mut self, fd: RawFd, emitted_ipc: &mut Vec<FilterAction>) {
        self.device_fds.retain(|&f| f != fd);
        let (released_keys, commit) = self.filter.clear_device(fd);
        if !released_keys.is_empty() && self.uinput_fd >= 0 {
            for code in released_keys {
                if code == KEY_RIGHTALT {
                    self.right_alt_forwarded = false;
                }
                self.emit_key(code, 0);
            }
            self.emit_syn();
        }
        if let Some(act) = commit {
            emitted_ipc.push(act);
        }
        unsafe { libc::close(fd) };
    }

    fn process_input_event(
        &mut self,
        device_id: RawFd,
        ev: &InputEvent,
        ipc_out: &mut Vec<FilterAction>,
    ) {
        if self.uinput_fd < 0 {
            return;
        }
        if ev.type_ == EV_KEY {
            let (side, action) = self.filter.process_key_for_device(device_id, ev.code, ev.value);
            if let Some(side_evt) = side {
                ipc_out.push(side_evt);
            }
            match action {
                FilterAction::Forward => {
                    if ev.code == KEY_RIGHTALT {
                        self.right_alt_forwarded = ev.value != 0;
                    }
                    self.emit_key(ev.code, ev.value);
                    self.emit_syn();
                }
                FilterAction::Drop => {}
                FilterAction::EmitActionCenter => {
                    self.emit_action_center_combo();
                }
                FilterAction::AltTabStep { backwards } => {
                    // Ensure RightAlt (AltGr) is released on uinput if Alt+Tab was triggered with it
                    if self.right_alt_forwarded {
                        self.right_alt_forwarded = false;
                        self.emit_key(KEY_RIGHTALT, 0);
                        self.emit_syn();
                    }
                    ipc_out.push(FilterAction::AltTabStep { backwards });
                }
                FilterAction::AltTabCancel => {
                    ipc_out.push(FilterAction::AltTabCancel);
                }
                FilterAction::AltTabCommit => {
                    ipc_out.push(FilterAction::AltTabCommit);
                }
                FilterAction::NavBack
                | FilterAction::NavForward
                | FilterAction::NavHome
                | FilterAction::CloseActiveWindow => {
                    ipc_out.push(action);
                }
            }
        } else if ev.type_ == EV_SYN && ev.code == SYN_REPORT {
            self.emit_syn();
        }
    }

    fn create_virtual_keyboard(&mut self) -> Result<(), String> {
        let open_uinput = || -> RawFd {
            let p1 = CString::new("/dev/uinput").unwrap();
            let fd = unsafe {
                libc::open(
                    p1.as_ptr(),
                    libc::O_WRONLY | libc::O_NONBLOCK | libc::O_CLOEXEC,
                )
            };
            if fd >= 0 {
                return fd;
            }
            let p2 = CString::new("/dev/input/uinput").unwrap();
            unsafe {
                libc::open(
                    p2.as_ptr(),
                    libc::O_WRONLY | libc::O_NONBLOCK | libc::O_CLOEXEC,
                )
            }
        };

        let mut fd = open_uinput();
        if fd < 0 && io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT) {
            let _ = Command::new("modprobe").arg("uinput").status();
            thread::sleep(Duration::from_millis(100));
            fd = open_uinput();
        }
        if fd < 0 {
            return Err(format!(
                "Unable to open /dev/uinput: {}",
                io::Error::last_os_error()
            ));
        }

        self.uinput_fd = fd;

        if unsafe { libc::ioctl(fd, UI_SET_EVBIT, EV_KEY as libc::c_int) } < 0
            || unsafe { libc::ioctl(fd, UI_SET_EVBIT, EV_SYN as libc::c_int) } < 0
        {
            self.destroy_virtual_keyboard();
            return Err("Failed to configure /dev/uinput event types.".to_string());
        }

        // Register only standard keyboard keycodes 1..=248 (never BTN_* 0x100..0x1ff)
        for code in 1..=248 {
            unsafe {
                libc::ioctl(fd, UI_SET_KEYBIT, code as libc::c_int);
            }
        }

        let mut setup = UinputSetup {
            id: InputId {
                bustype: BUS_USB,
                vendor: 0x1d6b,
                product: 0x5eb1,
                version: 1,
            },
            name: [0; 80],
            ff_effects_max: 0,
        };
        for (i, b) in VIRTUAL_KEYBOARD_NAME.bytes().take(79).enumerate() {
            setup.name[i] = b as libc::c_char;
        }

        if unsafe { libc::ioctl(fd, UI_DEV_SETUP, &setup as *const UinputSetup) } < 0
            || unsafe { libc::ioctl(fd, UI_DEV_CREATE) } < 0
        {
            self.destroy_virtual_keyboard();
            return Err("Failed to create virtual keyboard via /dev/uinput.".to_string());
        }

        Ok(())
    }

    fn destroy_virtual_keyboard(&mut self) {
        if self.uinput_fd >= 0 {
            unsafe {
                libc::ioctl(self.uinput_fd, UI_DEV_DESTROY);
                libc::close(self.uinput_fd);
            }
            self.uinput_fd = -1;
        }
    }

    fn wait_keys_released(fd: RawFd, max_wait: Duration) {
        let deadline = std::time::Instant::now() + max_wait;
        let mut key_state = [0u8; (KEY_MAX / 8) + 1];
        loop {
            key_state.fill(0);
            let rc = unsafe {
                libc::ioctl(
                    fd,
                    eviocgkey(key_state.len()),
                    key_state.as_mut_ptr() as *mut libc::c_void,
                )
            };
            if rc < 0 || !key_state.iter().any(|&b| b != 0) {
                break;
            }
            if std::time::Instant::now() >= deadline {
                break;
            }
            thread::sleep(Duration::from_millis(15));
        }
    }

    fn is_physical_keyboard(fd: RawFd, event_name: &str) -> bool {
        if let Ok(canon) = std::fs::canonicalize(format!("/sys/class/input/{event_name}/device")) {
            if canon.to_string_lossy().starts_with("/sys/devices/virtual/") {
                return false;
            }
        }
        let mut name_buf = [0u8; 256];
        if unsafe {
            libc::ioctl(
                fd,
                eviocgname(name_buf.len() - 1),
                name_buf.as_mut_ptr() as *mut libc::c_void,
            )
        } >= 0
        {
            let nul_pos = name_buf
                .iter()
                .position(|&b| b == 0)
                .unwrap_or(name_buf.len());
            if let Ok(name) = std::str::from_utf8(&name_buf[..nul_pos]) {
                if name == VIRTUAL_KEYBOARD_NAME {
                    return false;
                }
            }
        }

        let mut ev_bits = [0u8; (EV_MAX / 8) + 1];
        if unsafe {
            libc::ioctl(
                fd,
                eviocgbit(0, ev_bits.len()),
                ev_bits.as_mut_ptr() as *mut libc::c_void,
            )
        } < 0
        {
            return false;
        }
        if !test_bit(&ev_bits, EV_KEY as usize) {
            return false;
        }

        if test_bit(&ev_bits, EV_REL as usize) {
            let mut rel_bits = [0u8; (REL_MAX / 8) + 1];
            if unsafe {
                libc::ioctl(
                    fd,
                    eviocgbit(EV_REL as u32, rel_bits.len()),
                    rel_bits.as_mut_ptr() as *mut libc::c_void,
                )
            } >= 0
                && (test_bit(&rel_bits, REL_X) || test_bit(&rel_bits, REL_Y))
            {
                return false;
            }
        }

        if test_bit(&ev_bits, EV_ABS as usize) {
            let mut abs_bits = [0u8; (ABS_MAX / 8) + 1];
            if unsafe {
                libc::ioctl(
                    fd,
                    eviocgbit(EV_ABS as u32, abs_bits.len()),
                    abs_bits.as_mut_ptr() as *mut libc::c_void,
                )
            } >= 0
                && (test_bit(&abs_bits, ABS_X) || test_bit(&abs_bits, ABS_MT_POSITION_X))
            {
                return false;
            }
        }

        let mut key_bits = [0u8; (KEY_MAX / 8) + 1];
        if unsafe {
            libc::ioctl(
                fd,
                eviocgbit(EV_KEY as u32, key_bits.len()),
                key_bits.as_mut_ptr() as *mut libc::c_void,
            )
        } < 0
        {
            return false;
        }

        let main_kbd = (test_bit(&key_bits, KEY_A as usize)
            && test_bit(&key_bits, KEY_ENTER as usize))
            || (test_bit(&key_bits, KEY_TAB as usize)
                && (test_bit(&key_bits, KEY_LEFTALT as usize)
                    || test_bit(&key_bits, KEY_RIGHTALT as usize)));
        let hotkey_kbd = test_bit(&key_bits, KEY_LEFTMETA as usize)
            || test_bit(&key_bits, KEY_RIGHTMETA as usize)
            || test_bit(&key_bits, KEY_SYSRQ as usize)
            || test_bit(&key_bits, KEY_SWITCHVIDEOMODE as usize)
            || test_bit(&key_bits, KEY_SCREENLOCK as usize)
            || test_bit(&key_bits, KEY_CALC as usize)
            || test_bit(&key_bits, KEY_SEARCH as usize)
            || test_bit(&key_bits, KEY_SCALE as usize)
            || test_bit(&key_bits, KEY_DASHBOARD as usize)
            || test_bit(&key_bits, KEY_MENU as usize)
            || test_bit(&key_bits, KEY_HOMEPAGE as usize)
            || test_bit(&key_bits, KEY_PROG1 as usize)
            || test_bit(&key_bits, KEY_PROG2 as usize)
            || test_bit(&key_bits, KEY_PROG3 as usize)
            || test_bit(&key_bits, KEY_PROG4 as usize)
            || (KEY_F13..=KEY_F24).any(|k| test_bit(&key_bits, k as usize));
        main_kbd || hotkey_kbd
    }

    fn emit_key(&self, code: u16, value: i32) {
        let ev = InputEvent {
            time: libc::timeval {
                tv_sec: 0,
                tv_usec: 0,
            },
            type_: EV_KEY,
            code,
            value,
        };
        unsafe {
            let _ = libc::write(
                self.uinput_fd,
                &ev as *const InputEvent as *const libc::c_void,
                mem::size_of::<InputEvent>(),
            );
        }
    }

    fn emit_syn(&self) {
        let ev = InputEvent {
            time: libc::timeval {
                tv_sec: 0,
                tv_usec: 0,
            },
            type_: EV_SYN,
            code: SYN_REPORT,
            value: 0,
        };
        unsafe {
            let _ = libc::write(
                self.uinput_fd,
                &ev as *const InputEvent as *const libc::c_void,
                mem::size_of::<InputEvent>(),
            );
        }
    }

    fn emit_action_center_combo(&self) {
        let need_ctrl = !self.filter.ctrl_held();
        let need_shift = !self.filter.shift_held();

        if need_ctrl {
            self.emit_key(KEY_LEFTCTRL, 1);
        }
        if need_shift {
            self.emit_key(KEY_LEFTSHIFT, 1);
        }
        self.emit_key(KEY_A, 1);
        self.emit_syn();

        self.emit_key(KEY_A, 0);
        if need_shift {
            self.emit_key(KEY_LEFTSHIFT, 0);
        }
        if need_ctrl {
            self.emit_key(KEY_LEFTCTRL, 0);
        }
        self.emit_syn();
    }
}

impl Drop for KeyboardBlocker {
    fn drop(&mut self) {
        self.unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alt_tab_drives_internal_switcher_and_commits_on_alt_release() {
        let mut f = KeyFilterState::default();
        f.policy.allow_action_center_combo = true;

        // LeftAlt is NEVER emitted to uinput when OS Alt shortcuts are locked
        assert_eq!(f.process_key(KEY_LEFTALT, 1), (None, FilterAction::Drop));
        assert_eq!(
            f.process_key(KEY_TAB, 1),
            (None, FilterAction::AltTabStep { backwards: false })
        );
        assert_eq!(f.process_key(KEY_TAB, 0), (None, FilterAction::Drop));
        assert_eq!(
            f.process_key(KEY_LEFTALT, 0),
            (Some(FilterAction::AltTabCommit), FilterAction::Drop)
        );

        // Alt+Left / Alt+Right / Alt+Home translate to IPC nav actions without leaking LeftAlt
        assert_eq!(f.process_key(KEY_LEFTALT, 1), (None, FilterAction::Drop));
        assert_eq!(f.process_key(KEY_LEFT, 1), (None, FilterAction::NavBack));
        assert_eq!(f.process_key(KEY_LEFT, 0), (None, FilterAction::Drop));
        assert_eq!(f.process_key(KEY_RIGHT, 1), (None, FilterAction::NavForward));
        assert_eq!(f.process_key(KEY_RIGHT, 0), (None, FilterAction::Drop));
        assert_eq!(f.process_key(KEY_HOME, 1), (None, FilterAction::NavHome));
        assert_eq!(f.process_key(KEY_HOME, 0), (None, FilterAction::Drop));
        assert_eq!(f.process_key(KEY_LEFTALT, 0), (None, FilterAction::Drop));
    }

    #[test]
    fn super_a_translates_to_action_center_and_blocks_other_super() {
        let mut f = KeyFilterState::default();
        f.policy.allow_action_center_combo = true;

        assert_eq!(f.process_key(KEY_LEFTMETA, 1), (None, FilterAction::Drop));
        assert_eq!(
            f.process_key(KEY_A, 1),
            (None, FilterAction::EmitActionCenter)
        );
        assert_eq!(f.process_key(KEY_A, 0), (None, FilterAction::Drop));
        assert_eq!(f.process_key(KEY_LEFTMETA, 0), (None, FilterAction::Drop));
    }

    #[test]
    fn blocks_kde_alt_f3_and_ctrl_alt_tty_and_allows_typing() {
        let mut f = KeyFilterState::default();
        assert_eq!(f.process_key(KEY_A, 1), (None, FilterAction::Forward));
        assert_eq!(f.process_key(KEY_A, 0), (None, FilterAction::Forward));

        // Alt+F3 (KEY_F1 + 2 = 61)
        assert_eq!(f.process_key(KEY_LEFTALT, 1), (None, FilterAction::Drop));
        assert_eq!(f.process_key(KEY_F1 + 2, 1), (None, FilterAction::Drop));
        assert_eq!(f.process_key(KEY_F1 + 2, 0), (None, FilterAction::Drop));
        assert_eq!(f.process_key(KEY_LEFTALT, 0), (None, FilterAction::Drop));

        // Ctrl+Alt+F3
        assert_eq!(
            f.process_key(KEY_LEFTCTRL, 1),
            (None, FilterAction::Forward)
        );
        assert_eq!(f.process_key(KEY_LEFTALT, 1), (None, FilterAction::Drop));
        assert_eq!(f.process_key(KEY_F1 + 2, 1), (None, FilterAction::Drop));

        // Standalone F13..F24 system/launch keys
        let mut f2 = KeyFilterState::default();
        assert_eq!(f2.process_key(KEY_F13, 1), (None, FilterAction::Drop));
        assert_eq!(f2.process_key(KEY_F24, 1), (None, FilterAction::Drop));
    }

    #[test]
    fn multi_device_alt_tab_and_pregrab_held_alt_never_leak_tab() {
        let mut f = KeyFilterState::default();

        // Device 10 holds LeftAlt; spurious release on device 11 must NOT clear device 10's Alt
        assert_eq!(
            f.process_key_for_device(10, KEY_LEFTALT, 1),
            (None, FilterAction::Drop)
        );
        assert_eq!(
            f.process_key_for_device(11, KEY_LEFTALT, 0),
            (None, FilterAction::Drop)
        );
        assert_eq!(
            f.process_key_for_device(10, KEY_TAB, 1),
            (None, FilterAction::AltTabStep { backwards: false })
        );
        // Unplugging device 10 cleanly commits the active Alt+Tab session
        assert_eq!(f.clear_device(10).1, Some(FilterAction::AltTabCommit));

        // Pre-grab held Alt synced via EVIOCGKEY must catch Tab and drop orphan Alt release
        let mut f2 = KeyFilterState::default();
        let mut key_bits = [0u8; (KEY_MAX / 8) + 1];
        key_bits[(KEY_LEFTALT as usize) / 8] |= 1u8 << ((KEY_LEFTALT as usize) % 8);
        f2.sync_device_held_keys(20, &key_bits);

        assert_eq!(
            f2.process_key_for_device(20, KEY_TAB, 1),
            (None, FilterAction::AltTabStep { backwards: false })
        );
        assert_eq!(
            f2.process_key_for_device(20, KEY_LEFTALT, 0),
            (Some(FilterAction::AltTabCommit), FilterAction::Drop)
        );
    }

    #[test]
    fn hold_tab_then_press_alt_then_release_tab_never_sticks_tab() {
        let mut f = KeyFilterState::default();

        // 1. Hold TAB (forwarded to uinput)
        assert_eq!(f.process_key(KEY_TAB, 1), (None, FilterAction::Forward));
        // 2. Press LeftAlt while TAB is still held (LeftAlt dropped)
        assert_eq!(f.process_key(KEY_LEFTALT, 1), (None, FilterAction::Drop));
        // 3. Hardware TAB repeat while Alt is held
        assert_eq!(
            f.process_key(KEY_TAB, 2),
            (None, FilterAction::AltTabStep { backwards: false })
        );
        // 4. Release TAB while Alt is still held -> MUST forward KEY_TAB 0 so TAB stops repeating!
        assert_eq!(f.process_key(KEY_TAB, 0), (None, FilterAction::Forward));
        // 5. Release Alt -> commits switcher cleanly
        assert_eq!(
            f.process_key(KEY_LEFTALT, 0),
            (Some(FilterAction::AltTabCommit), FilterAction::Drop)
        );
    }

    #[test]
    fn clear_device_returns_released_forwarded_keys_excluding_other_devices() {
        let mut f = KeyFilterState::default();
        assert_eq!(
            f.process_key_for_device(10, KEY_A, 1),
            (None, FilterAction::Forward)
        );
        assert_eq!(
            f.process_key_for_device(10, KEY_LEFTCTRL, 1),
            (None, FilterAction::Forward)
        );
        // Device 11 also holds KEY_LEFTCTRL
        assert_eq!(
            f.process_key_for_device(11, KEY_LEFTCTRL, 1),
            (None, FilterAction::Forward)
        );

        // Unplugging device 10 should release KEY_A, but NOT KEY_LEFTCTRL (still held on device 11)
        let (released, commit) = f.clear_device(10);
        assert_eq!(commit, None);
        assert!(released.contains(&KEY_A));
        assert!(!released.contains(&KEY_LEFTCTRL));
    }
}
