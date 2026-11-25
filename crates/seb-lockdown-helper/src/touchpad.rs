use crate::keyboard::{eviocgbit, eviocgname, ior, test_bit, InputEvent, EV_ABS, EV_KEY, EV_SYN};
use crate::protocol::VIRTUAL_KEYBOARD_NAME;

use std::ffi::{CStr, CString};
use std::io;
use std::mem;
use std::os::fd::RawFd;
use std::thread;
use std::time::Duration;

const VIRTUAL_TOUCHPAD_NAME: &str = "SEB Safe Touchpad";
const VIRTUAL_TOUCHPAD_PHYS: &str = "seb-virtual-touchpad/input0";

const EV_MSC: u16 = 0x04;
const EV_MAX: usize = 0x1f;
const KEY_MAX: usize = 0x2ff;
const ABS_MAX: usize = 0x3f;
const MSC_MAX: usize = 0x07;
const INPUT_PROP_MAX: usize = 0x1f;

const INPUT_PROP_POINTER: usize = 0x00;
const INPUT_PROP_DIRECT: usize = 0x01;
const INPUT_PROP_BUTTONPAD: usize = 0x02;

pub const ABS_MT_SLOT: u16 = 0x2f;
pub const ABS_MT_FIRST: u16 = 0x30;
pub const ABS_MT_POSITION_X: u16 = 0x35;
pub const ABS_MT_POSITION_Y: u16 = 0x36;
#[cfg(test)]
pub const ABS_MT_TRACKING_ID: u16 = 0x39;
pub const ABS_MT_LAST: u16 = 0x3e;

pub const BTN_TOOL_FINGER: u16 = 0x145;
pub const BTN_TOOL_QUINTTAP: u16 = 0x148;
pub const BTN_TOOL_DOUBLETAP: u16 = 0x14d;
pub const BTN_TOOL_TRIPLETAP: u16 = 0x14e;
pub const BTN_TOOL_QUADTAP: u16 = 0x14f;
const SYN_REPORT: u16 = 0;

const BUS_USB: u16 = 0x03;

const EVIOCGRAB: libc::c_ulong = 0x4004_4590;
const UI_DEV_CREATE: libc::c_ulong = 0x5501;
const UI_DEV_DESTROY: libc::c_ulong = 0x5502;
const UI_SET_EVBIT: libc::c_ulong = 0x4004_5564;
const UI_SET_KEYBIT: libc::c_ulong = 0x4004_5565;
const UI_SET_ABSBIT: libc::c_ulong = 0x4004_5567;
const UI_SET_MSCBIT: libc::c_ulong = 0x4004_5568;
const UI_SET_PROPBIT: libc::c_ulong = 0x4004_556e;
const UI_DEV_SETUP: libc::c_ulong = 0x405c_5503;
const UI_ABS_SETUP: libc::c_ulong = 0x401c_5504;

const fn ui_set_phys_ioctl() -> libc::c_ulong {
    ((1u32 << 30)
        | ((mem::size_of::<*const libc::c_char>() as u32) << 16)
        | ((b'U' as u32) << 8)
        | 108) as libc::c_ulong
}

const fn eviocgid() -> libc::c_ulong {
    ior(b'E' as u32, 0x02, mem::size_of::<InputId>())
}

const fn eviocgphys(len: usize) -> libc::c_ulong {
    ior(b'E' as u32, 0x07, len)
}

const fn eviocgprop(len: usize) -> libc::c_ulong {
    ior(b'E' as u32, 0x09, len)
}

const fn eviocgabs(abs: u32) -> libc::c_ulong {
    ior(b'E' as u32, 0x40 + abs, mem::size_of::<InputAbsInfo>())
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
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

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct InputAbsInfo {
    value: i32,
    minimum: i32,
    maximum: i32,
    fuzz: i32,
    flat: i32,
    resolution: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct UinputAbsSetup {
    code: u16,
    absinfo: InputAbsInfo,
}

/// Stateful per-touchpad evdev filter that suppresses >=3 finger multi-touch gestures
/// (`BTN_TOOL_TRIPLETAP`, `BTN_TOOL_QUADTAP`, `BTN_TOOL_QUINTTAP`, and MT slots >= 2)
/// across full `SYN_REPORT` frames while keeping 1-finger and 2-finger input glitch-free.
#[derive(Debug, Default)]
pub struct TouchpadFilterState {
    hw_slot: i32,
    hw_double: bool,
    hw_triple: bool,
    hw_quad: bool,
    hw_quint: bool,
    virt_double: bool,
    frame_events: Vec<(u16, u16, i32)>,
}

impl TouchpadFilterState {
    pub fn filter_event(&mut self, ev: &InputEvent) -> Vec<(u16, u16, i32)> {
        match ev.type_ {
            EV_ABS => {
                if ev.code == ABS_MT_SLOT {
                    self.hw_slot = ev.value;
                    if (0..=1).contains(&self.hw_slot) {
                        self.frame_events.push((EV_ABS, ABS_MT_SLOT, self.hw_slot));
                    }
                } else if (ABS_MT_FIRST..=ABS_MT_LAST).contains(&ev.code) {
                    if (0..=1).contains(&self.hw_slot) {
                        self.frame_events.push((ev.type_, ev.code, ev.value));
                    }
                } else {
                    self.frame_events.push((ev.type_, ev.code, ev.value));
                }
                Vec::new()
            }
            EV_KEY => {
                match ev.code {
                    BTN_TOOL_DOUBLETAP => self.hw_double = ev.value != 0,
                    BTN_TOOL_TRIPLETAP => self.hw_triple = ev.value != 0,
                    BTN_TOOL_QUADTAP => self.hw_quad = ev.value != 0,
                    BTN_TOOL_QUINTTAP => self.hw_quint = ev.value != 0,
                    _ => self.frame_events.push((EV_KEY, ev.code, ev.value)),
                }
                Vec::new()
            }
            EV_MSC => {
                self.frame_events.push((ev.type_, ev.code, ev.value));
                Vec::new()
            }
            EV_SYN if ev.code == SYN_REPORT => {
                let want_double =
                    self.hw_double || self.hw_triple || self.hw_quad || self.hw_quint;
                if want_double != self.virt_double {
                    self.virt_double = want_double;
                    self.frame_events
                        .push((EV_KEY, BTN_TOOL_DOUBLETAP, i32::from(want_double)));
                }
                if self.frame_events.is_empty() {
                    return Vec::new();
                }
                let mut out = std::mem::take(&mut self.frame_events);
                out.push((EV_SYN, SYN_REPORT, 0));
                out
            }
            EV_SYN => vec![(ev.type_, ev.code, ev.value)],
            _ => Vec::new(),
        }
    }
}

struct GrabbedTouchpad {
    phys_fd: RawFd,
    uinput_fd: RawFd,
    filter: TouchpadFilterState,
}

/// Intercepts physical multi-touch touchpads during exam lockdown and proxies them through
/// a safe 2-finger-max virtual touchpad so OS desktop-switch/overview gestures cannot fire.
pub struct TouchpadBlocker {
    devices: Vec<GrabbedTouchpad>,
    locked: bool,
}

impl TouchpadBlocker {
    pub fn new() -> Self {
        Self {
            devices: Vec::new(),
            locked: false,
        }
    }

    pub fn is_locked(&self) -> bool {
        self.locked
    }

    pub fn grabbed_count(&self) -> usize {
        self.devices.len()
    }

    pub fn device_fds(&self) -> Vec<RawFd> {
        self.devices.iter().map(|d| d.phys_fd).collect()
    }

    pub fn lock(&mut self) -> Result<usize, String> {
        if self.locked {
            return Ok(self.devices.len());
        }

        let dir_path = CString::new("/dev/input").unwrap();
        let dir = unsafe { libc::opendir(dir_path.as_ptr()) };
        if dir.is_null() {
            return Err(format!("Unable to open /dev/input: {}", io::Error::last_os_error()));
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
            if !Self::is_physical_touchpad(fd, name_str) {
                unsafe { libc::close(fd) };
                continue;
            }
            match Self::create_virtual_touchpad(fd) {
                Ok(uinput_fd) => {
                    // Wait briefly so udev/libinput/compositor attaches the virtual touchpad
                    // and applies OS per-device settings (e.g. NaturalScroll) before EVIOCGRAB.
                    thread::sleep(Duration::from_millis(80));
                    if unsafe { libc::ioctl(fd, EVIOCGRAB, 1 as libc::c_int) } < 0 {
                        unsafe {
                            libc::ioctl(uinput_fd, UI_DEV_DESTROY);
                            libc::close(uinput_fd);
                            libc::close(fd);
                        }
                        continue;
                    }
                    self.devices.push(GrabbedTouchpad {
                        phys_fd: fd,
                        uinput_fd,
                        filter: TouchpadFilterState::default(),
                    });
                }
                Err(_) => unsafe {
                    libc::close(fd);
                },
            }
        }
        unsafe { libc::closedir(dir) };

        self.locked = !self.devices.is_empty();
        Ok(self.devices.len())
    }

    pub fn unlock(&mut self) {
        self.locked = false;
        for dev in self.devices.drain(..) {
            unsafe {
                libc::ioctl(dev.phys_fd, EVIOCGRAB, 0 as libc::c_int);
                libc::close(dev.phys_fd);
                libc::ioctl(dev.uinput_fd, UI_DEV_DESTROY);
                libc::close(dev.uinput_fd);
            }
        }
    }

    pub fn handle_readable_fd(&mut self, fd: RawFd) {
        let Some(idx) = self.devices.iter().position(|d| d.phys_fd == fd) else {
            return;
        };
        let mut events = [InputEvent::default(); 64];
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
                let code = io::Error::last_os_error().raw_os_error().unwrap_or(0);
                if code == libc::EAGAIN || code == libc::EWOULDBLOCK {
                    break;
                }
                if code == libc::ENODEV {
                    let dev = self.devices.remove(idx);
                    unsafe {
                        libc::close(dev.phys_fd);
                        libc::ioctl(dev.uinput_fd, UI_DEV_DESTROY);
                        libc::close(dev.uinput_fd);
                    }
                }
                break;
            }
            if bytes == 0 {
                break;
            }

            let count = (bytes as usize) / event_size;
            let dev = &mut self.devices[idx];
            for ev in &events[..count] {
                let mapped = dev.filter.filter_event(ev);
                for (t, c, v) in mapped {
                    let out_ev = InputEvent {
                        time: libc::timeval {
                            tv_sec: 0,
                            tv_usec: 0,
                        },
                        type_: t,
                        code: c,
                        value: v,
                    };
                    unsafe {
                        let _ = libc::write(
                            dev.uinput_fd,
                            &out_ev as *const InputEvent as *const libc::c_void,
                            event_size,
                        );
                    }
                }
            }
        }
    }

    fn is_physical_touchpad(fd: RawFd, event_name: &str) -> bool {
        // Never re-grab kernel virtual / uinput devices (we clone physical vendor/product/name
        // onto our uinput touchpad so OS compositors preserve NaturalScroll/TapToClick/etc.).
        if let Ok(canon) = std::fs::canonicalize(format!("/sys/class/input/{event_name}/device")) {
            if canon.to_string_lossy().starts_with("/sys/devices/virtual/") {
                return false;
            }
        }

        let mut phys_buf = [0u8; 256];
        if unsafe {
            libc::ioctl(
                fd,
                eviocgphys(phys_buf.len() - 1),
                phys_buf.as_mut_ptr() as *mut libc::c_void,
            )
        } >= 0
        {
            let nul_pos = phys_buf
                .iter()
                .position(|&b| b == 0)
                .unwrap_or(phys_buf.len());
            if let Ok(phys) = std::str::from_utf8(&phys_buf[..nul_pos]) {
                if phys.starts_with("seb-virtual-") {
                    return false;
                }
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
                if name == VIRTUAL_TOUCHPAD_NAME || name == VIRTUAL_KEYBOARD_NAME {
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
        if !test_bit(&ev_bits, EV_KEY as usize) || !test_bit(&ev_bits, EV_ABS as usize) {
            return false;
        }

        let mut prop_bits = [0u8; (INPUT_PROP_MAX / 8) + 1];
        if unsafe {
            libc::ioctl(
                fd,
                eviocgprop(prop_bits.len()),
                prop_bits.as_mut_ptr() as *mut libc::c_void,
            )
        } >= 0
            && test_bit(&prop_bits, INPUT_PROP_DIRECT)
        {
            return false;
        }

        let mut abs_bits = [0u8; (ABS_MAX / 8) + 1];
        if unsafe {
            libc::ioctl(
                fd,
                eviocgbit(EV_ABS as u32, abs_bits.len()),
                abs_bits.as_mut_ptr() as *mut libc::c_void,
            )
        } < 0
        {
            return false;
        }
        if !test_bit(&abs_bits, ABS_MT_POSITION_X as usize)
            || !test_bit(&abs_bits, ABS_MT_POSITION_Y as usize)
        {
            return false;
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

        test_bit(&key_bits, BTN_TOOL_FINGER as usize)
            || test_bit(&prop_bits, INPUT_PROP_POINTER)
            || test_bit(&prop_bits, INPUT_PROP_BUTTONPAD)
    }

    fn create_virtual_touchpad(phys_fd: RawFd) -> Result<RawFd, String> {
        let path = CString::new("/dev/uinput").unwrap();
        let ufd = unsafe {
            libc::open(
                path.as_ptr(),
                libc::O_WRONLY | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        if ufd < 0 {
            return Err(format!("Unable to open /dev/uinput: {}", io::Error::last_os_error()));
        }

        let cleanup_err = |msg: &str| -> Result<RawFd, String> {
            unsafe { libc::close(ufd) };
            Err(msg.to_string())
        };

        if let Ok(c_phys) = CString::new(VIRTUAL_TOUCHPAD_PHYS) {
            unsafe {
                libc::ioctl(ufd, ui_set_phys_ioctl(), c_phys.as_ptr());
            }
        }

        let mut phys_ev_bits = [0u8; (EV_MAX / 8) + 1];
        let has_ev_bits = unsafe {
            libc::ioctl(
                phys_fd,
                eviocgbit(0, phys_ev_bits.len()),
                phys_ev_bits.as_mut_ptr() as *mut libc::c_void,
            )
        } >= 0;

        if unsafe { libc::ioctl(ufd, UI_SET_EVBIT, EV_SYN as libc::c_int) } < 0
            || unsafe { libc::ioctl(ufd, UI_SET_EVBIT, EV_KEY as libc::c_int) } < 0
            || unsafe { libc::ioctl(ufd, UI_SET_EVBIT, EV_ABS as libc::c_int) } < 0
        {
            return cleanup_err("Failed to enable uinput event bits for touchpad.");
        }
        if has_ev_bits && test_bit(&phys_ev_bits, EV_MSC as usize) {
            let mut msc_bits = [0u8; (MSC_MAX / 8) + 1];
            if unsafe {
                libc::ioctl(
                    phys_fd,
                    eviocgbit(EV_MSC as u32, msc_bits.len()),
                    msc_bits.as_mut_ptr() as *mut libc::c_void,
                )
            } >= 0
                && unsafe { libc::ioctl(ufd, UI_SET_EVBIT, EV_MSC as libc::c_int) } >= 0
            {
                for code in 0..=MSC_MAX as u16 {
                    if test_bit(&msc_bits, code as usize) {
                        unsafe {
                            libc::ioctl(ufd, UI_SET_MSCBIT, code as libc::c_int);
                        }
                    }
                }
            }
        }

        let mut key_bits = [0u8; (KEY_MAX / 8) + 1];
        if unsafe {
            libc::ioctl(
                phys_fd,
                eviocgbit(EV_KEY as u32, key_bits.len()),
                key_bits.as_mut_ptr() as *mut libc::c_void,
            )
        } >= 0
        {
            for code in 0x100..=0x1ffu16 {
                if matches!(
                    code,
                    BTN_TOOL_TRIPLETAP | BTN_TOOL_QUADTAP | BTN_TOOL_QUINTTAP
                ) {
                    continue;
                }
                if test_bit(&key_bits, code as usize) {
                    unsafe {
                        libc::ioctl(ufd, UI_SET_KEYBIT, code as libc::c_int);
                    }
                }
            }
        }

        let mut prop_bits = [0u8; (INPUT_PROP_MAX / 8) + 1];
        if unsafe {
            libc::ioctl(
                phys_fd,
                eviocgprop(prop_bits.len()),
                prop_bits.as_mut_ptr() as *mut libc::c_void,
            )
        } >= 0
        {
            for prop in 0..=INPUT_PROP_MAX {
                if prop != INPUT_PROP_DIRECT && test_bit(&prop_bits, prop) {
                    unsafe {
                        libc::ioctl(ufd, UI_SET_PROPBIT, prop as libc::c_int);
                    }
                }
            }
        }

        let mut abs_bits = [0u8; (ABS_MAX / 8) + 1];
        if unsafe {
            libc::ioctl(
                phys_fd,
                eviocgbit(EV_ABS as u32, abs_bits.len()),
                abs_bits.as_mut_ptr() as *mut libc::c_void,
            )
        } >= 0
        {
            for axis in 0..=ABS_MAX as u16 {
                if !test_bit(&abs_bits, axis as usize) {
                    continue;
                }
                let mut info = InputAbsInfo::default();
                if unsafe {
                    libc::ioctl(
                        phys_fd,
                        eviocgabs(axis as u32),
                        &mut info as *mut InputAbsInfo as *mut libc::c_void,
                    )
                } < 0
                {
                    continue;
                }
                if axis == ABS_MT_SLOT && info.maximum > 1 {
                    info.maximum = 1;
                    info.value = info.value.clamp(info.minimum, info.maximum);
                }
                unsafe {
                    libc::ioctl(ufd, UI_SET_ABSBIT, axis as libc::c_int);
                }
                let setup = UinputAbsSetup {
                    code: axis,
                    absinfo: info,
                };
                if unsafe { libc::ioctl(ufd, UI_ABS_SETUP, &setup as *const UinputAbsSetup) } < 0 {
                    return cleanup_err("UI_ABS_SETUP failed for virtual touchpad.");
                }
            }
        }

        // Preserve the physical touchpad's exact InputId (bustype/vendor/product/version) and
        // device name so KDE/GNOME/Sway/Xorg match per-device OS settings (NaturalScroll,
        // ScrollFactor, TapToClick, PointerAcceleration, DisableWhileTyping).
        let mut phys_id = InputId {
            bustype: BUS_USB,
            vendor: 0x1d6b,
            product: 0x5eb2,
            version: 1,
        };
        let _ = unsafe {
            libc::ioctl(
                phys_fd,
                eviocgid(),
                &mut phys_id as *mut InputId as *mut libc::c_void,
            )
        };

        let mut phys_name = String::new();
        let mut name_buf = [0u8; 256];
        if unsafe {
            libc::ioctl(
                phys_fd,
                eviocgname(name_buf.len() - 1),
                name_buf.as_mut_ptr() as *mut libc::c_void,
            )
        } >= 0
        {
            let nul_pos = name_buf
                .iter()
                .position(|&b| b == 0)
                .unwrap_or(name_buf.len());
            if let Ok(s) = std::str::from_utf8(&name_buf[..nul_pos]) {
                if !s.trim().is_empty() {
                    phys_name = s.to_string();
                }
            }
        }
        let target_name = if phys_name.is_empty() {
            VIRTUAL_TOUCHPAD_NAME
        } else {
            phys_name.as_str()
        };

        let mut dev_setup = UinputSetup {
            id: phys_id,
            name: [0; 80],
            ff_effects_max: 0,
        };
        for (i, b) in target_name.bytes().take(79).enumerate() {
            dev_setup.name[i] = b as libc::c_char;
        }
        if unsafe { libc::ioctl(ufd, UI_DEV_SETUP, &dev_setup as *const UinputSetup) } < 0
            || unsafe { libc::ioctl(ufd, UI_DEV_CREATE) } < 0
        {
            return cleanup_err("UI_DEV_CREATE failed for virtual touchpad.");
        }

        Ok(ufd)
    }
}

impl Drop for TouchpadBlocker {
    fn drop(&mut self) {
        self.unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(type_: u16, code: u16, value: i32) -> InputEvent {
        InputEvent {
            time: libc::timeval {
                tv_sec: 0,
                tv_usec: 0,
            },
            type_,
            code,
            value,
        }
    }

    #[test]
    fn suppresses_three_and_four_finger_touchpad_gestures_but_keeps_two_finger_scroll() {
        let mut f = TouchpadFilterState::default();

        // 2-finger frame: slots 0 & 1 + DOUBLETAP 1 + SYN -> emitted cleanly.
        assert!(f.filter_event(&mk(EV_ABS, ABS_MT_SLOT, 0)).is_empty());
        assert!(f.filter_event(&mk(EV_ABS, ABS_MT_TRACKING_ID, 10)).is_empty());
        assert!(f.filter_event(&mk(EV_ABS, ABS_MT_SLOT, 1)).is_empty());
        assert!(f.filter_event(&mk(EV_ABS, ABS_MT_TRACKING_ID, 11)).is_empty());
        assert!(f.filter_event(&mk(EV_KEY, BTN_TOOL_DOUBLETAP, 1)).is_empty());
        assert_eq!(
            f.filter_event(&mk(EV_SYN, SYN_REPORT, 0)),
            vec![
                (EV_ABS, ABS_MT_SLOT, 0),
                (EV_ABS, ABS_MT_TRACKING_ID, 10),
                (EV_ABS, ABS_MT_SLOT, 1),
                (EV_ABS, ABS_MT_TRACKING_ID, 11),
                (EV_KEY, BTN_TOOL_DOUBLETAP, 1),
                (EV_SYN, SYN_REPORT, 0),
            ]
        );

        // 3rd/4th finger frame: slot 2 dropped, DOUBLETAP 0 + TRIPLETAP 1 stays DOUBLETAP 1 (no glitch!).
        assert!(f.filter_event(&mk(EV_ABS, ABS_MT_SLOT, 2)).is_empty());
        assert!(f.filter_event(&mk(EV_ABS, ABS_MT_TRACKING_ID, 12)).is_empty());
        assert!(f.filter_event(&mk(EV_ABS, ABS_MT_POSITION_X, 500)).is_empty());
        assert!(f.filter_event(&mk(EV_KEY, BTN_TOOL_DOUBLETAP, 0)).is_empty());
        assert!(f.filter_event(&mk(EV_KEY, BTN_TOOL_TRIPLETAP, 1)).is_empty());
        assert!(f.filter_event(&mk(EV_SYN, SYN_REPORT, 0)).is_empty());

        // Switching to 4-finger swipe also emits nothing extra.
        assert!(f.filter_event(&mk(EV_KEY, BTN_TOOL_TRIPLETAP, 0)).is_empty());
        assert!(f.filter_event(&mk(EV_KEY, BTN_TOOL_QUADTAP, 1)).is_empty());
        assert!(f.filter_event(&mk(EV_SYN, SYN_REPORT, 0)).is_empty());

        // Lifting all extra fingers emits clean BTN_TOOL_DOUBLETAP 0 + SYN_REPORT.
        assert!(f.filter_event(&mk(EV_KEY, BTN_TOOL_QUADTAP, 0)).is_empty());
        assert_eq!(
            f.filter_event(&mk(EV_SYN, SYN_REPORT, 0)),
            vec![(EV_KEY, BTN_TOOL_DOUBLETAP, 0), (EV_SYN, SYN_REPORT, 0)]
        );
    }
}
