pub mod audio;
pub use audio::*;

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::process::Command;
use x11rb::connection::Connection as X11Connection;
use x11rb::protocol::xkb::{self, ConnectionExt as XkbConnectionExt};
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as X11ConnectionExt, ModMask};
use zbus::blocking::Connection as DBusConnection;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BatteryState {
    pub available: bool,
    pub power_connected: bool,
    pub charging: bool,
    pub percentage: i32,
    pub time_remaining: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct KeyboardLayoutState {
    pub current_short_name: String,
    pub current_long_name: String,
    pub available_layouts: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WifiNetwork {
    pub ssid: String,
    pub signal_percent: i32,
    pub active: bool,
    #[serde(default)]
    pub secured: bool,
    #[serde(default)]
    pub saved: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NetworkState {
    pub available: bool,
    /// 0 = None, 1 = Wired, 2 = Wireless
    pub kind: i32,
    /// 0 = Disconnected, 1 = Connecting, 2 = Connected
    pub status: i32,
    pub active_connection: String,
    pub networks: Vec<WifiNetwork>,
}

fn run_cmd(program: &str, args: &[&str]) -> String {
    Command::new(program)
        .args(args)
        .env("LC_ALL", "C")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

fn dbus_get_prop(
    conn: &DBusConnection,
    dest: &str,
    path: &str,
    iface: &str,
    prop: &str,
) -> Option<OwnedValue> {
    let reply = conn
        .call_method(
            Some(dest),
            path,
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &(iface, prop),
        )
        .ok()?;
    reply.body().deserialize::<OwnedValue>().ok()
}

fn format_minutes(mins: i64) -> String {
    if mins <= 0 {
        return String::new();
    }
    if mins >= 60 {
        let h = mins as f64 / 60.0;
        format!("{h:.1} hours")
    } else {
        format!("{mins} minutes")
    }
}

fn read_sysfs_u64(dir: &std::path::Path, file: &str) -> Option<u64> {
    fs::read_to_string(dir.join(file))
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
}

fn query_battery_sysfs() -> Option<BatteryState> {
    let rd = fs::read_dir("/sys/class/power_supply").ok()?;
    let mut state = BatteryState::default();
    let mut mains_online = None;

    for entry in rd.flatten() {
        let path = entry.path();
        let kind = fs::read_to_string(path.join("type")).unwrap_or_default();
        let kind = kind.trim();
        let scope = fs::read_to_string(path.join("scope")).unwrap_or_default();
        if scope.trim().eq_ignore_ascii_case("Device") {
            continue;
        }
        if kind.eq_ignore_ascii_case("Mains") {
            if let Ok(online) = fs::read_to_string(path.join("online")) {
                let is_on = online.trim() == "1";
                mains_online = Some(mains_online.unwrap_or(false) || is_on);
            }
            continue;
        }
        if !kind.eq_ignore_ascii_case("Battery") || state.available {
            continue;
        }
        let present = fs::read_to_string(path.join("present"))
            .map(|s| s.trim() != "0")
            .unwrap_or(true);
        if !present {
            continue;
        }
        let cap = read_sysfs_u64(&path, "capacity").or_else(|| {
            let now = read_sysfs_u64(&path, "energy_now").or_else(|| read_sysfs_u64(&path, "charge_now"))?;
            let full = read_sysfs_u64(&path, "energy_full").or_else(|| read_sysfs_u64(&path, "charge_full"))?;
            (full > 0).then(|| (now.saturating_mul(100) / full).min(100))
        });
        let Some(pct) = cap else { continue };
        let status = fs::read_to_string(path.join("status")).unwrap_or_default();
        let st = status.trim();
        state.available = true;
        state.percentage = (pct as i32).clamp(0, 100);
        state.charging = st.eq_ignore_ascii_case("Charging");
        state.power_connected = state.charging
            || st.eq_ignore_ascii_case("Full")
            || st.eq_ignore_ascii_case("Not charging");

        if let Some(rate) = read_sysfs_u64(&path, "power_now").or_else(|| read_sysfs_u64(&path, "current_now")) {
            if rate > 0 {
                let now = read_sysfs_u64(&path, "energy_now").or_else(|| read_sysfs_u64(&path, "charge_now"));
                let full = read_sysfs_u64(&path, "energy_full").or_else(|| read_sysfs_u64(&path, "charge_full"));
                let rem_mins = if state.charging {
                    now.zip(full)
                        .and_then(|(n, f)| f.checked_sub(n))
                        .map(|diff| ((diff as f64 / rate as f64) * 60.0).round() as i64)
                } else {
                    now.map(|n| ((n as f64 / rate as f64) * 60.0).round() as i64)
                };
                if let Some(m) = rem_mins {
                    state.time_remaining = format_minutes(m);
                }
            }
        }
    }

    if let Some(on) = mains_online {
        state.power_connected = on;
    }
    state.available.then_some(state)
}

fn query_battery_upower_dbus() -> Option<BatteryState> {
    let conn = DBusConnection::system().ok()?;
    let reply = conn
        .call_method(
            Some("org.freedesktop.UPower"),
            "/org/freedesktop/UPower/devices/DisplayDevice",
            Some("org.freedesktop.DBus.Properties"),
            "GetAll",
            &("org.freedesktop.UPower.Device",),
        )
        .ok()?;
    let props = reply.body().deserialize::<HashMap<String, OwnedValue>>().ok()?;
    let is_present = props
        .get("IsPresent")
        .and_then(|v| v.downcast_ref::<bool>().ok())
        .unwrap_or(false);
    if !is_present {
        return None;
    }
    let pct = props
        .get("Percentage")
        .and_then(|v| v.downcast_ref::<f64>().ok())
        .map(|p| p.round() as i32)
        .unwrap_or(0)
        .clamp(0, 100);
    let up_state = props
        .get("State")
        .and_then(|v| v.downcast_ref::<u32>().ok())
        .unwrap_or(0);
    // UPower DeviceState: 1=Charging, 2=Discharging, 4=FullyCharged, 5=PendingCharge
    let charging = up_state == 1 || up_state == 5;
    let power_connected = charging || up_state == 4;
    let secs = if charging {
        props
            .get("TimeToFull")
            .and_then(|v| v.downcast_ref::<i64>().ok())
            .unwrap_or(0)
    } else {
        props
            .get("TimeToEmpty")
            .and_then(|v| v.downcast_ref::<i64>().ok())
            .unwrap_or(0)
    };
    Some(BatteryState {
        available: true,
        power_connected,
        charging,
        percentage: pct,
        time_remaining: format_minutes(secs / 60),
    })
}

pub fn parse_battery_snapshot(upower_output: &str) -> BatteryState {
    let mut next = BatteryState::default();
    if !upower_output.is_empty() {
        let mut absent = false;
        let pct_re = Regex::new(r"([0-9]+(?:\.[0-9]+)?)%").ok();
        for line in upower_output.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("percentage:") {
                if let Some(re) = &pct_re {
                    if let Some(caps) = re.captures(trimmed) {
                        if let Ok(pct) = caps[1].parse::<f64>() {
                            if (0.0..=100.0).contains(&pct) {
                                next.percentage = pct.round() as i32;
                                next.available = true;
                            }
                        }
                    }
                }
            } else if let Some(val) = trimmed.strip_prefix("present:") {
                absent = val.trim() == "no";
            } else if let Some(val) = trimmed.strip_prefix("state:") {
                let v = val.trim();
                next.charging = v == "charging" || v == "pending-charge";
                next.power_connected = next.charging || v == "fully-charged";
            } else if let Some(val) = trimmed
                .strip_prefix("time to empty:")
                .or_else(|| trimmed.strip_prefix("time to full:"))
            {
                next.time_remaining = val.trim().to_string();
            }
        }
        if absent {
            next.available = false;
        }
    }

    let mut found_supply = false;
    let mut online = false;
    if let Ok(entries) = fs::read_dir("/sys/class/power_supply") {
        for entry in entries.flatten() {
            if let Ok(val) = fs::read_to_string(entry.path().join("online")) {
                found_supply = true;
                online |= val.trim() == "1";
            }
        }
    }
    if found_supply {
        next.power_connected = online;
    }
    next
}

pub fn query_battery() -> BatteryState {
    // 1. Kernel sysfs direct (universal across all Linux distros, no daemon required)
    if let Some(state) = query_battery_sysfs() {
        return state;
    }
    // 2. UPower over D-Bus
    if let Some(state) = query_battery_upower_dbus() {
        return state;
    }
    // 3. `upower` CLI fallback
    let output = run_cmd("upower", &["-i", "/org/freedesktop/UPower/devices/DisplayDevice"]);
    parse_battery_snapshot(&output)
}

fn build_layout_state(raw_layouts: &str, variant: &str) -> KeyboardLayoutState {
    let available_layouts: Vec<String> = raw_layouts
        .trim()
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let trimmed_var = variant.trim();
    let current_short_name = available_layouts
        .first()
        .map(|s| s.to_uppercase())
        .unwrap_or_default();
    let current_long_name = if !trimmed_var.is_empty() {
        trimmed_var.to_string()
    } else {
        available_layouts.first().cloned().unwrap_or_default()
    };
    KeyboardLayoutState {
        current_short_name,
        current_long_name,
        available_layouts,
    }
}

fn query_keyboard_dbus_locale1() -> Option<KeyboardLayoutState> {
    let conn = DBusConnection::system().ok()?;
    let layout = dbus_get_prop(
        &conn,
        "org.freedesktop.locale1",
        "/org/freedesktop/locale1",
        "org.freedesktop.locale1",
        "X11Layout",
    )
    .and_then(|v| v.downcast_ref::<String>().ok())?;
    if layout.trim().is_empty() {
        return None;
    }
    let variant = dbus_get_prop(
        &conn,
        "org.freedesktop.locale1",
        "/org/freedesktop/locale1",
        "org.freedesktop.locale1",
        "X11Variant",
    )
    .and_then(|v| v.downcast_ref::<String>().ok())
    .unwrap_or_default();
    Some(build_layout_state(&layout, &variant))
}

fn query_keyboard_x11() -> Option<KeyboardLayoutState> {
    let (conn, screen_num) = x11rb::connect(None).ok()?;
    let root = conn.setup().roots.get(screen_num)?.root;
    let atom = conn
        .intern_atom(true, b"_XKB_RULES_NAMES")
        .ok()?
        .reply()
        .ok()?
        .atom;
    if atom == 0 {
        return None;
    }
    let prop = conn
        .get_property(false, root, atom, AtomEnum::STRING, 0, 1024)
        .ok()?
        .reply()
        .ok()?;
    // Null-separated: rules \0 model \0 layout \0 variant \0 options
    let fields: Vec<String> = prop
        .value
        .split(|&b| b == 0)
        .map(|s| String::from_utf8_lossy(s).to_string())
        .collect();
    let layouts = fields.get(2).map(String::as_str).unwrap_or("");
    if layouts.trim().is_empty() {
        return None;
    }
    let variant = fields.get(3).map(String::as_str).unwrap_or("");
    Some(build_layout_state(layouts, variant))
}

fn query_keyboard_config_files() -> Option<KeyboardLayoutState> {
    if let Ok(rd) = fs::read_dir("/etc/X11/xorg.conf.d") {
        let layout_re = Regex::new(r#"(?i)Option\s+"XkbLayout"\s+"([^"]+)""#).ok()?;
        let variant_re = Regex::new(r#"(?i)Option\s+"XkbVariant"\s+"([^"]+)""#).ok()?;
        for entry in rd.flatten() {
            if entry.path().extension().and_then(|e| e.to_str()) == Some("conf") {
                if let Ok(txt) = fs::read_to_string(entry.path()) {
                    if let Some(caps) = layout_re.captures(&txt) {
                        let var = variant_re
                            .captures(&txt)
                            .and_then(|v| v.get(1))
                            .map(|m| m.as_str())
                            .unwrap_or("");
                        let st = build_layout_state(&caps[1], var);
                        if !st.available_layouts.is_empty() {
                            return Some(st);
                        }
                    }
                }
            }
        }
    }
    if let Ok(vc) = fs::read_to_string("/etc/vconsole.conf") {
        for line in vc.lines() {
            if let Some(val) = line.trim().strip_prefix("KEYMAP=") {
                let clean = val.trim().trim_matches('"');
                if !clean.is_empty() {
                    return Some(build_layout_state(clean, ""));
                }
            }
        }
    }
    None
}

pub fn query_keyboard_layout() -> KeyboardLayoutState {
    if let Some(st) = query_keyboard_dbus_locale1() {
        if !st.available_layouts.is_empty() {
            return st;
        }
    }
    if let Some(st) = query_keyboard_x11() {
        if !st.available_layouts.is_empty() {
            return st;
        }
    }
    if let Ok(env_layout) = std::env::var("XKB_DEFAULT_LAYOUT") {
        let st = build_layout_state(&env_layout, "");
        if !st.available_layouts.is_empty() {
            return st;
        }
    }
    if let Some(st) = query_keyboard_config_files() {
        return st;
    }
    let output = run_cmd("localectl", &["status"]);
    parse_localectl(&output)
}

pub fn parse_localectl(output: &str) -> KeyboardLayoutState {
    let mut layouts = String::new();
    let mut variant = String::new();
    for line in output.lines() {
        let trimmed = line.trim();
        if let Some(val) = trimmed.strip_prefix("X11 Layout:") {
            layouts = val.trim().to_string();
        } else if let Some(val) = trimmed.strip_prefix("X11 Variant:") {
            variant = val.trim().to_string();
        }
    }
    build_layout_state(&layouts, &variant)
}

pub fn split_nmcli_fields(line: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut field = String::new();
    let mut escaped = false;
    for ch in line.chars() {
        if escaped {
            field.push(ch);
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == ':' {
            result.push(std::mem::take(&mut field));
        } else {
            field.push(ch);
        }
    }
    if escaped {
        field.push('\\');
    }
    result.push(field);
    result
}

pub fn parse_nmcli_network(device_output: &str, wifi_output: &str) -> NetworkState {
    let mut next = NetworkState::default();
    let mut selected_rank = -1;
    for line in device_output.lines() {
        if line.is_empty() {
            continue;
        }
        let parts = split_nmcli_fields(line);
        if parts.len() != 4 {
            continue;
        }
        let dev_type = parts[1].as_str();
        if dev_type != "wifi" && dev_type != "ethernet" {
            continue;
        }
        let status = if parts[2].starts_with("connected") {
            2
        } else if parts[2].contains("connecting") {
            1
        } else {
            0
        };
        let rank = match status {
            2 => 10,
            1 => 5,
            _ => 0,
        } + if dev_type == "wifi" { 2 } else { 1 };
        if rank <= selected_rank {
            continue;
        }
        selected_rank = rank;
        next.available = true;
        next.kind = if dev_type == "wifi" { 2 } else { 1 };
        next.active_connection = parts[3].clone();
        next.status = status;
    }

    if next.kind == 2 {
        for line in wifi_output.lines() {
            if line.is_empty() {
                continue;
            }
            let parts = split_nmcli_fields(line);
            if parts.len() != 3 || parts[1].is_empty() {
                continue;
            }
            if let Ok(signal) = parts[2].parse::<i32>() {
                if (0..=100).contains(&signal) {
                    next.networks.push(WifiNetwork {
                        ssid: parts[1].clone(),
                        signal_percent: signal,
                        active: parts[0] == "yes",
                        secured: true,
                        saved: false,
                    });
                }
            }
        }
    }
    next
}

fn nm_saved_wifi_connections(conn: &DBusConnection) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Ok(reply) = conn.call_method(
        Some("org.freedesktop.NetworkManager"),
        "/org/freedesktop/NetworkManager/Settings",
        Some("org.freedesktop.NetworkManager.Settings"),
        "ListConnections",
        &(),
    ) else {
        return out;
    };
    let Ok(paths) = reply.body().deserialize::<Vec<OwnedObjectPath>>() else {
        return out;
    };
    for p in paths {
        let Ok(cfg_msg) = conn.call_method(
            Some("org.freedesktop.NetworkManager"),
            p.as_str(),
            Some("org.freedesktop.NetworkManager.Settings.Connection"),
            "GetSettings",
            &(),
        ) else {
            continue;
        };
        let Ok(cfg) = cfg_msg
            .body()
            .deserialize::<HashMap<String, HashMap<String, OwnedValue>>>()
        else {
            continue;
        };
        if let Some(w) = cfg.get("802-11-wireless") {
            if let Some(ssid_val) = w.get("ssid") {
                if let Ok(bytes) = Vec::<u8>::try_from(ssid_val.clone()) {
                    let ssid = String::from_utf8_lossy(&bytes).trim_matches('\0').to_string();
                    if !ssid.is_empty() {
                        out.push((ssid, p.as_str().to_string()));
                    }
                }
            }
        }
    }
    out
}

fn query_network_dbus_nm() -> Option<NetworkState> {
    let conn = DBusConnection::system().ok()?;
    let devices_val = dbus_get_prop(
        &conn,
        "org.freedesktop.NetworkManager",
        "/org/freedesktop/NetworkManager",
        "org.freedesktop.NetworkManager",
        "AllDevices",
    )?;
    let devices = Vec::<OwnedObjectPath>::try_from(devices_val).ok()?;

    let mut next = NetworkState::default();
    let mut selected_rank = -1;
    let mut selected_wifi_path: Option<String> = None;

    for dev_path in devices {
        let p = dev_path.as_str();
        let dev_type = dbus_get_prop(
            &conn,
            "org.freedesktop.NetworkManager",
            p,
            "org.freedesktop.NetworkManager.Device",
            "DeviceType",
        )
        .and_then(|v| v.downcast_ref::<u32>().ok())
        .unwrap_or(0);
        // NM_DEVICE_TYPE_ETHERNET = 1, NM_DEVICE_TYPE_WIFI = 2
        if dev_type != 1 && dev_type != 2 {
            continue;
        }
        let nm_state = dbus_get_prop(
            &conn,
            "org.freedesktop.NetworkManager",
            p,
            "org.freedesktop.NetworkManager.Device",
            "State",
        )
        .and_then(|v| v.downcast_ref::<u32>().ok())
        .unwrap_or(0);
        // NM_DEVICE_STATE_ACTIVATED = 100, 40..=90 = PREPARE..SECONDARIES
        let status = if nm_state == 100 {
            2
        } else if (40..=90).contains(&nm_state) {
            1
        } else {
            0
        };
        let rank = match status {
            2 => 10,
            1 => 5,
            _ => 0,
        } + if dev_type == 2 { 2 } else { 1 };
        if rank <= selected_rank {
            continue;
        }
        selected_rank = rank;
        next.available = true;
        next.kind = if dev_type == 2 { 2 } else { 1 };
        next.status = status;
        next.active_connection = dbus_get_prop(
            &conn,
            "org.freedesktop.NetworkManager",
            p,
            "org.freedesktop.NetworkManager.Device",
            "ActiveConnection",
        )
        .and_then(|v| OwnedObjectPath::try_from(v).ok())
        .filter(|ap| ap.as_str() != "/")
        .and_then(|ap| {
            dbus_get_prop(
                &conn,
                "org.freedesktop.NetworkManager",
                ap.as_str(),
                "org.freedesktop.NetworkManager.Connection.Active",
                "Id",
            )
            .and_then(|v| v.downcast_ref::<String>().ok())
        })
        .unwrap_or_default();
        if dev_type == 2 {
            selected_wifi_path = Some(p.to_string());
        } else {
            selected_wifi_path = None;
        }
    }

    if let Some(wifi_dev) = selected_wifi_path {
        let saved_ssids: HashSet<String> = nm_saved_wifi_connections(&conn)
            .into_iter()
            .map(|(s, _)| s)
            .collect();
        let active_ap = dbus_get_prop(
            &conn,
            "org.freedesktop.NetworkManager",
            &wifi_dev,
            "org.freedesktop.NetworkManager.Device.Wireless",
            "ActiveAccessPoint",
        )
        .and_then(|v| OwnedObjectPath::try_from(v).ok())
        .map(|o| o.as_str().to_string())
        .unwrap_or_default();

        if let Some(aps_val) = dbus_get_prop(
            &conn,
            "org.freedesktop.NetworkManager",
            &wifi_dev,
            "org.freedesktop.NetworkManager.Device.Wireless",
            "AccessPoints",
        ) {
            if let Ok(aps) = Vec::<OwnedObjectPath>::try_from(aps_val) {
                let mut seen: HashMap<String, usize> = HashMap::new();
                for ap in aps {
                    let ap_str = ap.as_str();
                    let ssid_bytes = dbus_get_prop(
                        &conn,
                        "org.freedesktop.NetworkManager",
                        ap_str,
                        "org.freedesktop.NetworkManager.AccessPoint",
                        "Ssid",
                    )
                    .and_then(|v| Vec::<u8>::try_from(v).ok())
                    .unwrap_or_default();
                    let ssid = String::from_utf8_lossy(&ssid_bytes)
                        .trim_matches('\0')
                        .trim()
                        .to_string();
                    if ssid.is_empty() {
                        continue;
                    }
                    let signal = dbus_get_prop(
                        &conn,
                        "org.freedesktop.NetworkManager",
                        ap_str,
                        "org.freedesktop.NetworkManager.AccessPoint",
                        "Strength",
                    )
                    .and_then(|v| v.downcast_ref::<u8>().ok())
                    .unwrap_or(0) as i32;
                    let flags = dbus_get_prop(
                        &conn,
                        "org.freedesktop.NetworkManager",
                        ap_str,
                        "org.freedesktop.NetworkManager.AccessPoint",
                        "Flags",
                    )
                    .and_then(|v| v.downcast_ref::<u32>().ok())
                    .unwrap_or(0);
                    let wpa_flags = dbus_get_prop(
                        &conn,
                        "org.freedesktop.NetworkManager",
                        ap_str,
                        "org.freedesktop.NetworkManager.AccessPoint",
                        "WpaFlags",
                    )
                    .and_then(|v| v.downcast_ref::<u32>().ok())
                    .unwrap_or(0);
                    let rsn_flags = dbus_get_prop(
                        &conn,
                        "org.freedesktop.NetworkManager",
                        ap_str,
                        "org.freedesktop.NetworkManager.AccessPoint",
                        "RsnFlags",
                    )
                    .and_then(|v| v.downcast_ref::<u32>().ok())
                    .unwrap_or(0);
                    let secured = (flags & 0x1) != 0 || wpa_flags != 0 || rsn_flags != 0;
                    let is_active = ap_str == active_ap;
                    if let Some(&idx) = seen.get(&ssid) {
                        if is_active {
                            next.networks[idx].active = true;
                        }
                        if signal > next.networks[idx].signal_percent {
                            next.networks[idx].signal_percent = signal;
                        }
                        next.networks[idx].secured |= secured;
                    } else {
                        let saved = saved_ssids.contains(&ssid);
                        seen.insert(ssid.clone(), next.networks.len());
                        next.networks.push(WifiNetwork {
                            ssid,
                            signal_percent: signal.clamp(0, 100),
                            active: is_active,
                            secured,
                            saved,
                        });
                    }
                }
            }
        }
        next.networks.sort_by(|a, b| {
            b.active
                .cmp(&a.active)
                .then_with(|| b.saved.cmp(&a.saved))
                .then_with(|| b.signal_percent.cmp(&a.signal_percent))
        });
    }

    next.available.then_some(next)
}

fn query_network_sysfs_fallback() -> NetworkState {
    let mut next = NetworkState::default();
    let Ok(rd) = fs::read_dir("/sys/class/net") else {
        return next;
    };
    let wireless_proc = fs::read_to_string("/proc/net/wireless").unwrap_or_default();

    let mut best_rank = -1;
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name == "lo" || name.starts_with("veth") || name.starts_with("docker") {
            continue;
        }
        let path = entry.path();
        let is_wifi = path.join("wireless").is_dir();
        let up = fs::read_to_string(path.join("operstate"))
            .map(|s| s.trim() == "up")
            .unwrap_or(false);
        let status = if up { 2 } else { 0 };
        let rank = (if up { 10 } else { 0 }) + if is_wifi { 2 } else { 1 };
        if rank > best_rank {
            best_rank = rank;
            next.available = true;
            next.kind = if is_wifi { 2 } else { 1 };
            next.status = status;
            next.active_connection = if up { name.clone() } else { String::new() };
        }
        if is_wifi && up {
            let mut sig_pct = 75;
            for line in wireless_proc.lines().skip(2) {
                if let Some((iface, rest)) = line.split_once(':') {
                    if iface.trim() == name {
                        if let Some(link) = rest.split_whitespace().nth(1) {
                            if let Ok(q) = link.trim_end_matches('.').parse::<f64>() {
                                sig_pct = ((q / 70.0) * 100.0).round().clamp(0.0, 100.0) as i32;
                            }
                        }
                    }
                }
            }
            next.networks.push(WifiNetwork {
                ssid: name,
                signal_percent: sig_pct,
                active: true,
                secured: true,
                saved: true,
            });
        }
    }
    next
}

type IwdManagedObjects = HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>>;

fn dbm_hundredths_to_percent(rssi: i16) -> i32 {
    let dbm = (rssi as f64) / 100.0;
    ((dbm + 100.0) * 2.0).round().clamp(0.0, 100.0) as i32
}

fn query_network_dbus_iwd() -> Option<NetworkState> {
    let conn = DBusConnection::system().ok()?;
    let reply = conn
        .call_method(
            Some("net.connman.iwd"),
            "/",
            Some("org.freedesktop.DBus.ObjectManager"),
            "GetManagedObjects",
            &(),
        )
        .ok()?;
    let objects = reply.body().deserialize::<IwdManagedObjects>().ok()?;

    let mut station_path: Option<String> = None;
    let mut station_state = String::new();
    let mut net_names: HashMap<String, (String, bool)> = HashMap::new();

    for (path, ifaces) in &objects {
        if let Some(st) = ifaces.get("net.connman.iwd.Station") {
            station_path = Some(path.as_str().to_string());
            station_state = st
                .get("State")
                .and_then(|v| v.downcast_ref::<String>().ok())
                .unwrap_or_default();
        }
        if let Some(net) = ifaces.get("net.connman.iwd.Network") {
            let name = net
                .get("Name")
                .and_then(|v| v.downcast_ref::<String>().ok())
                .unwrap_or_default();
            let connected = net
                .get("Connected")
                .and_then(|v| v.downcast_ref::<bool>().ok())
                .unwrap_or(false);
            if !name.is_empty() {
                net_names.insert(path.as_str().to_string(), (name, connected));
            }
        }
    }

    let station = station_path?;
    let status = match station_state.as_str() {
        "connected" => 2,
        "connecting" | "roaming" => 1,
        _ => 0,
    };
    let mut next = NetworkState {
        available: true,
        kind: 2,
        status,
        active_connection: String::new(),
        networks: Vec::new(),
    };

    if let Ok(ordered_msg) = conn.call_method(
        Some("net.connman.iwd"),
        station.as_str(),
        Some("net.connman.iwd.Station"),
        "GetOrderedNetworks",
        &(),
    ) {
        if let Ok(ordered) = ordered_msg.body().deserialize::<Vec<(OwnedObjectPath, i16)>>() {
            for (obj, rssi) in ordered {
                if let Some((ssid, connected)) = net_names.get(obj.as_str()) {
                    if *connected && next.active_connection.is_empty() {
                        next.active_connection = ssid.clone();
                    }
                    next.networks.push(WifiNetwork {
                        ssid: ssid.clone(),
                        signal_percent: dbm_hundredths_to_percent(rssi),
                        active: *connected,
                        secured: true,
                        saved: *connected,
                    });
                }
            }
        }
    }
    Some(next)
}

fn connect_wifi_dbus_iwd(ssid: &str) -> bool {
    let Ok(conn) = DBusConnection::system() else {
        return false;
    };
    let Ok(reply) = conn.call_method(
        Some("net.connman.iwd"),
        "/",
        Some("org.freedesktop.DBus.ObjectManager"),
        "GetManagedObjects",
        &(),
    ) else {
        return false;
    };
    let Ok(objects) = reply.body().deserialize::<IwdManagedObjects>() else {
        return false;
    };
    for (path, ifaces) in objects {
        if let Some(net) = ifaces.get("net.connman.iwd.Network") {
            let name = net
                .get("Name")
                .and_then(|v| v.downcast_ref::<String>().ok())
                .unwrap_or_default();
            if name == ssid {
                return conn
                    .call_method(
                        Some("net.connman.iwd"),
                        path.as_str(),
                        Some("net.connman.iwd.Network"),
                        "Connect",
                        &(),
                    )
                    .is_ok();
            }
        }
    }
    false
}

pub fn query_network() -> NetworkState {
    // 1. NetworkManager via Pure-Rust D-Bus
    if let Some(st) = query_network_dbus_nm() {
        return st;
    }
    // 2. iwd (net.connman.iwd) via Pure-Rust D-Bus
    if let Some(st) = query_network_dbus_iwd() {
        return st;
    }
    // 3. `nmcli` CLI fallback
    let dev = run_cmd(
        "nmcli",
        &["-t", "-f", "DEVICE,TYPE,STATE,CONNECTION", "device"],
    );
    if !dev.is_empty() {
        let wifi = run_cmd(
            "nmcli",
            &[
                "-t",
                "-f",
                "ACTIVE,SSID,SIGNAL",
                "device",
                "wifi",
                "list",
                "--rescan",
                "no",
            ],
        );
        return parse_nmcli_network(&dev, &wifi);
    }
    // 3. Universal kernel `/sys/class/net` + `/proc/net/wireless` fallback (iwd / systemd-networkd / connman)
    query_network_sysfs_fallback()
}

pub fn request_wifi_scan() {
    if let Ok(conn) = DBusConnection::system() {
        if let Some(devices_val) = dbus_get_prop(
            &conn,
            "org.freedesktop.NetworkManager",
            "/org/freedesktop/NetworkManager",
            "org.freedesktop.NetworkManager",
            "AllDevices",
        ) {
            if let Ok(devices) = Vec::<OwnedObjectPath>::try_from(devices_val) {
                for p in devices {
                    if dbus_get_prop(
                        &conn,
                        "org.freedesktop.NetworkManager",
                        p.as_str(),
                        "org.freedesktop.NetworkManager.Device",
                        "DeviceType",
                    )
                    .and_then(|v| v.downcast_ref::<u32>().ok())
                        == Some(2)
                    {
                        let empty: HashMap<&str, Value<'_>> = HashMap::new();
                        let _ = conn.call_method(
                            Some("org.freedesktop.NetworkManager"),
                            p.as_str(),
                            Some("org.freedesktop.NetworkManager.Device.Wireless"),
                            "RequestScan",
                            &(empty,),
                        );
                    }
                }
            }
        }
        if let Ok(reply) = conn.call_method(
            Some("net.connman.iwd"),
            "/",
            Some("org.freedesktop.DBus.ObjectManager"),
            "GetManagedObjects",
            &(),
        ) {
            if let Ok(objects) = reply.body().deserialize::<IwdManagedObjects>() {
                for (path, ifaces) in objects {
                    if ifaces.contains_key("net.connman.iwd.Station") {
                        let _ = conn.call_method(
                            Some("net.connman.iwd"),
                            path.as_str(),
                            Some("net.connman.iwd.Station"),
                            "Scan",
                            &(),
                        );
                    }
                }
            }
        }
    }
}

fn find_best_nm_ap(conn: &DBusConnection, wifi_dev: &str, ssid: &str) -> (String, &'static str) {
    let mut best_ap = "/".to_string();
    let mut best_sig = -1i32;
    let mut best_key_mgmt: &'static str = "wpa-psk";

    if let Some(aps_val) = dbus_get_prop(
        conn,
        "org.freedesktop.NetworkManager",
        wifi_dev,
        "org.freedesktop.NetworkManager.Device.Wireless",
        "AccessPoints",
    ) {
        if let Ok(aps) = Vec::<OwnedObjectPath>::try_from(aps_val) {
            for ap in aps {
                let ap_str = ap.as_str();
                let ssid_bytes = dbus_get_prop(
                    conn,
                    "org.freedesktop.NetworkManager",
                    ap_str,
                    "org.freedesktop.NetworkManager.AccessPoint",
                    "Ssid",
                )
                .and_then(|v| Vec::<u8>::try_from(v).ok())
                .unwrap_or_default();
                let cur_ssid = String::from_utf8_lossy(&ssid_bytes)
                    .trim_matches('\0')
                    .trim()
                    .to_string();
                if cur_ssid != ssid {
                    continue;
                }
                let sig = dbus_get_prop(
                    conn,
                    "org.freedesktop.NetworkManager",
                    ap_str,
                    "org.freedesktop.NetworkManager.AccessPoint",
                    "Strength",
                )
                .and_then(|v| v.downcast_ref::<u8>().ok())
                .unwrap_or(0) as i32;
                let rsn = dbus_get_prop(
                    conn,
                    "org.freedesktop.NetworkManager",
                    ap_str,
                    "org.freedesktop.NetworkManager.AccessPoint",
                    "RsnFlags",
                )
                .and_then(|v| v.downcast_ref::<u32>().ok())
                .unwrap_or(0);
                // NM_802_11_AP_SEC_KEY_MGMT_PSK = 0x100, NM_802_11_AP_SEC_KEY_MGMT_SAE = 0x400
                if sig > best_sig {
                    best_sig = sig;
                    best_ap = ap_str.to_string();
                    best_key_mgmt = if (rsn & 0x400) != 0 && (rsn & 0x100) == 0 {
                        "sae"
                    } else {
                        "wpa-psk"
                    };
                }
            }
        }
    }
    (best_ap, best_key_mgmt)
}

fn wait_nm_activation(conn: &DBusConnection, active_path: &str, wifi_dev: &str) -> Result<(), String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while std::time::Instant::now() < deadline {
        let ac_state = dbus_get_prop(
            conn,
            "org.freedesktop.NetworkManager",
            active_path,
            "org.freedesktop.NetworkManager.Connection.Active",
            "State",
        )
        .and_then(|v| v.downcast_ref::<u32>().ok());
        match ac_state {
            // NM_ACTIVE_CONNECTION_STATE_ACTIVATED = 2
            Some(2) => return Ok(()),
            // 3 = Deactivating, 4 = Deactivated, or None (object disappeared on failure)
            Some(3) | Some(4) | None => {
                let dev_state = dbus_get_prop(
                    conn,
                    "org.freedesktop.NetworkManager",
                    wifi_dev,
                    "org.freedesktop.NetworkManager.Device",
                    "State",
                )
                .and_then(|v| v.downcast_ref::<u32>().ok())
                .unwrap_or(0);
                let reason = dbus_get_prop(
                    conn,
                    "org.freedesktop.NetworkManager",
                    wifi_dev,
                    "org.freedesktop.NetworkManager.Device",
                    "StateReason",
                )
                .and_then(|v| <(u32, u32)>::try_from(v).ok())
                .map(|(_, r)| r)
                .unwrap_or(0);
                // Reasons 7 (NO_SECRETS), 8 (SUPPLICANT_DISCONNECT), 9, 10, 11 (auth/supplicant failures)
                if matches!(reason, 7..=11) {
                    return Err("Authentication failed. Please check the Wi-Fi password and try again.".to_string());
                }
                if dev_state == 120 || ac_state == Some(4) || ac_state.is_none() {
                    return Err("Unable to connect to the selected Wi-Fi network.".to_string());
                }
            }
            _ => {}
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    Err("Wi-Fi connection timed out.".to_string())
}

fn connect_wifi_dbus_nm(ssid: &str, password: Option<&str>) -> Result<(), String> {
    let conn = DBusConnection::system().map_err(|e| format!("D-Bus unavailable: {e}"))?;
    let devices_val = dbus_get_prop(
        &conn,
        "org.freedesktop.NetworkManager",
        "/org/freedesktop/NetworkManager",
        "org.freedesktop.NetworkManager",
        "AllDevices",
    )
    .ok_or_else(|| "NetworkManager not active.".to_string())?;
    let devices =
        Vec::<OwnedObjectPath>::try_from(devices_val).map_err(|_| "No NM devices.".to_string())?;
    let wifi_dev = devices
        .into_iter()
        .find(|p| {
            dbus_get_prop(
                &conn,
                "org.freedesktop.NetworkManager",
                p.as_str(),
                "org.freedesktop.NetworkManager.Device",
                "DeviceType",
            )
            .and_then(|v| v.downcast_ref::<u32>().ok())
                == Some(2)
        })
        .ok_or_else(|| "No Wi-Fi adapter found.".to_string())?;

    let (ap_path_str, key_mgmt) = find_best_nm_ap(&conn, wifi_dev.as_str(), ssid);
    let specific_ap = ObjectPath::try_from(ap_path_str.as_str())
        .or_else(|_| ObjectPath::try_from("/"))
        .map_err(|e| e.to_string())?;

    let saved = nm_saved_wifi_connections(&conn)
        .into_iter()
        .find(|(s, _)| s == ssid)
        .map(|(_, path)| path);

    let clean_pw = password.filter(|p| !p.is_empty());

    if let Some(saved_path) = saved {
        if let Some(pw) = clean_pw {
            if let Ok(cfg_msg) = conn.call_method(
                Some("org.freedesktop.NetworkManager"),
                saved_path.as_str(),
                Some("org.freedesktop.NetworkManager.Settings.Connection"),
                "GetSettings",
                &(),
            ) {
                if let Ok(mut cfg) = cfg_msg
                    .body()
                    .deserialize::<HashMap<String, HashMap<String, OwnedValue>>>()
                {
                    let sec = cfg
                        .entry("802-11-wireless-security".to_string())
                        .or_default();
                    if let Ok(v) = OwnedValue::try_from(Value::from(key_mgmt)) {
                        sec.insert("key-mgmt".to_string(), v);
                    }
                    if let Ok(v) = OwnedValue::try_from(Value::from(pw)) {
                        sec.insert("psk".to_string(), v);
                    }
                    let _ = conn.call_method(
                        Some("org.freedesktop.NetworkManager"),
                        saved_path.as_str(),
                        Some("org.freedesktop.NetworkManager.Settings.Connection"),
                        "Update",
                        &(cfg,),
                    );
                }
            }
        }
        let saved_obj = ObjectPath::try_from(saved_path.as_str()).map_err(|e| e.to_string())?;
        let active_msg = conn
            .call_method(
                Some("org.freedesktop.NetworkManager"),
                "/org/freedesktop/NetworkManager",
                Some("org.freedesktop.NetworkManager"),
                "ActivateConnection",
                &(saved_obj, wifi_dev.as_ref(), specific_ap),
            )
            .map_err(|e| format!("NetworkManager rejected activation: {e}"))?;
        if let Ok(active_path) = active_msg.body().deserialize::<OwnedObjectPath>() {
            return wait_nm_activation(&conn, active_path.as_str(), wifi_dev.as_str());
        }
        return Ok(());
    }

    let mut conn_sec: HashMap<&str, Value<'_>> = HashMap::new();
    conn_sec.insert("type", Value::from("802-11-wireless"));
    conn_sec.insert("id", Value::from(ssid));

    let mut wifi_sec: HashMap<&str, Value<'_>> = HashMap::new();
    wifi_sec.insert("ssid", Value::from(ssid.as_bytes().to_vec()));
    wifi_sec.insert("mode", Value::from("infrastructure"));

    let mut settings: HashMap<&str, HashMap<&str, Value<'_>>> = HashMap::new();
    settings.insert("connection", conn_sec);
    settings.insert("802-11-wireless", wifi_sec);

    if let Some(pw) = clean_pw {
        let mut sec: HashMap<&str, Value<'_>> = HashMap::new();
        sec.insert("key-mgmt", Value::from(key_mgmt));
        sec.insert("psk", Value::from(pw));
        settings.insert("802-11-wireless-security", sec);
    }

    let reply = conn
        .call_method(
            Some("org.freedesktop.NetworkManager"),
            "/org/freedesktop/NetworkManager",
            Some("org.freedesktop.NetworkManager"),
            "AddAndActivateConnection",
            &(settings, wifi_dev.as_ref(), specific_ap),
        )
        .map_err(|e| format!("Failed to add/activate Wi-Fi connection: {e}"))?;
    if let Ok((_, active_path)) = reply
        .body()
        .deserialize::<(OwnedObjectPath, OwnedObjectPath)>()
    {
        return wait_nm_activation(&conn, active_path.as_str(), wifi_dev.as_str());
    }
    Ok(())
}

pub fn connect_wifi(ssid: &str, password: Option<&str>) -> Result<(), String> {
    let nm_running = DBusConnection::system()
        .ok()
        .and_then(|c| {
            c.call_method(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                Some("org.freedesktop.DBus"),
                "NameHasOwner",
                &("org.freedesktop.NetworkManager",),
            )
            .ok()
        })
        .and_then(|r| r.body().deserialize::<bool>().ok())
        .unwrap_or(false);

    if nm_running {
        match connect_wifi_dbus_nm(ssid, password) {
            Ok(()) => return Ok(()),
            Err(nm_err) => {
                // Retry once via `nmcli` CLI only if D-Bus wasn't an auth failure
                if !nm_err.contains("Authentication failed") {
                    let mut args = vec!["device", "wifi", "connect", ssid];
                    if let Some(pw) = password.filter(|p| !p.is_empty()) {
                        args.push("password");
                        args.push(pw);
                    }
                    if let Ok(out) = Command::new("nmcli").args(&args).output() {
                        if out.status.success() {
                            return Ok(());
                        }
                    }
                }
                return Err(nm_err);
            }
        }
    }

    if connect_wifi_dbus_iwd(ssid) {
        return Ok(());
    }
    Err("No supported Wi-Fi manager (NetworkManager or iwd) could connect to this network.".to_string())
}

fn switch_keyboard_x11_xkb(index: usize) -> bool {
    let Ok((conn, _)) = x11rb::connect(None) else {
        return false;
    };
    let Some(ext) = conn.xkb_use_extension(1, 0).ok().and_then(|c| c.reply().ok()) else {
        return false;
    };
    if !ext.supported {
        return false;
    }
    let grp = match index {
        0 => xkb::Group::M1,
        1 => xkb::Group::M2,
        2 => xkb::Group::M3,
        _ => xkb::Group::M4,
    };
    conn.xkb_latch_lock_state(
        xkb::ID::USE_CORE_KBD.into(),
        ModMask::from(0u8),
        ModMask::from(0u8),
        true,
        grp,
        ModMask::from(0u8),
        false,
        0,
    )
    .ok()
    .and_then(|v| v.check().ok())
    .is_some()
}

/// Switches the active Linux keyboard layout across KDE D-Bus, GNOME, Hyprland, Sway, and X11.
pub fn switch_keyboard_layout(layout_code: &str, index: usize) -> bool {
    let clean = layout_code.trim();
    if clean.is_empty() {
        return false;
    }
    // 1. KDE Plasma Wayland/X11 over pure-Rust D-Bus (no `qdbus` needed)
    if let Ok(sess) = DBusConnection::session() {
        if let Ok(reply) = sess.call_method(
            Some("org.kde.keyboard"),
            "/Layouts",
            Some("org.kde.KeyboardLayouts"),
            "setLayout",
            &(index as u32,),
        ) {
            if reply.body().deserialize::<bool>().unwrap_or(true) {
                return true;
            }
        }
    }
    // 2. `qdbus` fallback (older Plasma variants)
    if Command::new("qdbus")
        .args([
            "org.kde.keyboard",
            "/Layouts",
            "setLayout",
            &index.to_string(),
        ])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        return true;
    }
    // 3. GNOME Wayland/X11
    if Command::new("gsettings")
        .args([
            "set",
            "org.gnome.desktop.input-sources",
            "current",
            &index.to_string(),
        ])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        return true;
    }
    // 4. Hyprland
    if Command::new("hyprctl")
        .args(["switchxkblayout", "all", &index.to_string()])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        return true;
    }
    // 5. Sway
    if Command::new("swaymsg")
        .args(["input", "type:keyboard", "xkb_switch_layout", &index.to_string()])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        return true;
    }
    // 6. Pure-Rust X11 XKB group lock
    if switch_keyboard_x11_xkb(index) {
        return true;
    }
    // 7. X11 setxkbmap fallback
    Command::new("setxkbmap")
        .arg(clean)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Non-blocking background platform monitor matching C++ intervals:
/// Audio (3s), Network (5s), Battery (10s), Keyboard (15s).
#[derive(Debug, Clone, Default)]
pub struct PlatformSnapshot {
    pub audio: AudioState,
    pub battery: BatteryState,
    pub keyboard: KeyboardLayoutState,
    pub network: NetworkState,
}

pub struct PlatformMonitor {
    state: std::sync::Arc<std::sync::Mutex<PlatformSnapshot>>,
    running: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl PlatformMonitor {
    pub fn start() -> Self {
        let state = std::sync::Arc::new(std::sync::Mutex::new(PlatformSnapshot::default()));
        let running = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));

        let st_c = std::sync::Arc::clone(&state);
        let run_c = std::sync::Arc::clone(&running);
        std::thread::spawn(move || {
            let mut tick: u64 = 0;
            while run_c.load(std::sync::atomic::Ordering::Relaxed) {
                if tick.is_multiple_of(3) {
                    let a = audio::query_audio();
                    if let Ok(mut g) = st_c.lock() {
                        g.audio = a;
                    }
                }
                if tick.is_multiple_of(5) {
                    let n = query_network();
                    if let Ok(mut g) = st_c.lock() {
                        g.network = n;
                    }
                }
                if tick.is_multiple_of(10) {
                    let b = query_battery();
                    if let Ok(mut g) = st_c.lock() {
                        g.battery = b;
                    }
                }
                if tick.is_multiple_of(15) {
                    let k = query_keyboard_layout();
                    if let Ok(mut g) = st_c.lock() {
                        g.keyboard = k;
                    }
                }
                tick = tick.wrapping_add(1);
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        });

        Self { state, running }
    }

    pub fn snapshot(&self) -> PlatformSnapshot {
        self.state.lock().map(|g| g.clone()).unwrap_or_default()
    }

    pub fn refresh_network_now(&self) -> PlatformSnapshot {
        let n = query_network();
        if let Ok(mut g) = self.state.lock() {
            g.network = n;
            return g.clone();
        }
        PlatformSnapshot::default()
    }

    pub fn update_audio_cached(&self, muted: Option<bool>, volume: Option<i32>) {
        if let Ok(mut g) = self.state.lock() {
            if let Some(m) = muted {
                g.audio.muted = m;
            }
            if let Some(v) = volume {
                g.audio.volume_percent = v.clamp(0, 100);
            }
        }
    }

    pub fn update_keyboard_cached(&self, short: &str) {
        if let Ok(mut g) = self.state.lock() {
            g.keyboard.current_short_name = short.to_string();
            g.keyboard.current_long_name = short.to_ascii_uppercase();
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CameraPortalOutcome {
    Granted,
    Cancelled,
    Denied,
    NoCamera,
    Unavailable,
}

/// Pre-flight camera permission request via the standard `org.freedesktop.portal.Camera`
/// D-Bus portal (`AccessCamera`). Calling this BEFORE entering fullscreen / kernel lockdown
/// ensures any first-time OS privacy prompt (e.g. KDE / GNOME portal) is answered cleanly
/// before the locked exam session begins. If permission was already granted previously,
/// the portal returns `Granted` immediately without showing a dialog.
pub fn request_camera_portal_permission() -> CameraPortalOutcome {
    let Ok(conn) = DBusConnection::session() else {
        return CameraPortalOutcome::Unavailable;
    };

    // Skip immediately if the portal reports no camera hardware present.
    if let Some(val) = dbus_get_prop(
        &conn,
        "org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.Camera",
        "IsCameraPresent",
    ) {
        if let Ok(false) = bool::try_from(val) {
            return CameraPortalOutcome::NoCamera;
        }
    }

    let Some(unique) = conn.unique_name() else {
        return CameraPortalOutcome::Unavailable;
    };
    let sender_segment = unique.as_str().trim_start_matches(':').replace('.', "_");
    let token = format!("seb_cam_{}", std::process::id());
    let expected_path = format!("/org/freedesktop/portal/desktop/request/{sender_segment}/{token}");

    let Ok(req_proxy) = zbus::blocking::Proxy::new(
        &conn,
        "org.freedesktop.portal.Desktop",
        expected_path.as_str(),
        "org.freedesktop.portal.Request",
    ) else {
        return CameraPortalOutcome::Unavailable;
    };
    let Ok(mut signals) = req_proxy.receive_signal("Response") else {
        return CameraPortalOutcome::Unavailable;
    };

    let mut options: HashMap<&str, Value<'_>> = HashMap::new();
    options.insert("handle_token", Value::from(token.as_str()));

    if conn
        .call_method(
            Some("org.freedesktop.portal.Desktop"),
            "/org/freedesktop/portal/desktop",
            Some("org.freedesktop.portal.Camera"),
            "AccessCamera",
            &(options,),
        )
        .is_err()
    {
        return CameraPortalOutcome::Unavailable;
    }

    if let Some(msg) = signals.next() {
        if let Ok((code, _results)) = msg.body().deserialize::<(u32, HashMap<String, OwnedValue>)>()
        {
            return match code {
                0 => CameraPortalOutcome::Granted,
                1 => CameraPortalOutcome::Cancelled,
                _ => CameraPortalOutcome::Denied,
            };
        }
    }
    CameraPortalOutcome::Unavailable
}

impl Drop for PlatformMonitor {
    fn drop(&mut self) {
        self.running
            .store(false, std::sync::atomic::Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_wpctl_and_nmcli_and_localectl() {
        let audio = parse_wpctl_volume("Volume: 0.65 [MUTED]");
        assert!(audio.available && audio.muted && audio.volume_percent == 65);

        let kb = parse_localectl("   X11 Layout: us,id\n   X11 Variant: intl\n");
        assert_eq!(kb.current_short_name, "US");
        assert_eq!(kb.current_long_name, "intl");

        let net = parse_nmcli_network(
            "wlan0:wifi:connected:Campus\\:WiFi\n",
            "yes:Campus\\:WiFi:82\n",
        );
        assert!(net.available);
        assert_eq!(net.active_connection, "Campus:WiFi");
        assert_eq!(net.networks.len(), 1);
        assert_eq!(net.networks[0].signal_percent, 82);
    }
}
