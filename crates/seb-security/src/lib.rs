use regex::{Regex, RegexBuilder};
pub mod lockdown_client;
pub mod power;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionRestriction {
    pub major: i32,
    pub minor: i32,
    pub patch: Option<i32>,
    pub build: Option<i32>,
    pub is_minimum_restriction: bool,
    pub requires_alliance_edition: bool,
}

fn parse_version(version: &str) -> Option<[i32; 4]> {
    let re = Regex::new(r"^([0-9]+)\.([0-9]+)\.([0-9]+)\.([0-9]+)$").ok()?;
    let caps = re.captures(version.trim())?;
    let mut out = [0i32; 4];
    for (idx, slot) in out.iter_mut().enumerate() {
        let v = caps.get(idx + 1)?.as_str().parse::<i32>().ok()?;
        if v < 0 {
            return None;
        }
        *slot = v;
    }
    Some(out)
}

pub fn validate_version_restrictions(
    restrictions: &[VersionRestriction],
    version: &str,
    alliance_edition: bool,
) -> bool {
    if restrictions.is_empty() {
        return true;
    }
    let Some([major, minor, patch, build]) = parse_version(version) else {
        return false;
    };

    restrictions.iter().any(|r| {
        if r.requires_alliance_edition && !alliance_edition {
            return false;
        }
        if !r.is_minimum_restriction {
            if major != r.major || minor != r.minor {
                return false;
            }
            if let Some(p) = r.patch {
                if patch != p {
                    return false;
                }
            }
            if let Some(b) = r.build {
                if build != b {
                    return false;
                }
            }
            return true;
        }

        let mut ok = r.major <= major;
        if r.major == major {
            ok &= r.minor <= minor;
            if r.minor == minor {
                ok &= r.patch.map_or(true, |p| p <= patch);
                if r.patch == Some(patch) {
                    ok &= r.build.map_or(true, |b| b <= build);
                }
            }
        }
        ok
    })
}

fn read_trimmed(path: &str) -> String {
    fs::read_to_string(path)
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

pub fn is_virtual_machine() -> bool {
    let cpu = read_trimmed("/proc/cpuinfo");
    if let Ok(re) = RegexBuilder::new(r"\bhypervisor\b")
        .case_insensitive(true)
        .build()
    {
        if re.is_match(&cpu) {
            return true;
        }
    }
    let hardware = format!(
        "{} {}",
        read_trimmed("/sys/class/dmi/id/sys_vendor"),
        read_trimmed("/sys/class/dmi/id/product_name")
    )
    .to_lowercase();
    for vendor in [
        "vmware",
        "virtualbox",
        "qemu",
        "kvm",
        "bochs",
        "xen",
        "parallels",
        "virtual machine",
    ] {
        if hardware.contains(vendor) {
            return true;
        }
    }
    false
}

pub fn is_debugger_attached() -> bool {
    let status = read_trimmed("/proc/self/status");
    let Ok(re) = Regex::new(r"(?m)^TracerPid:\s*(\d+)") else {
        return false;
    };
    re.captures(&status)
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse::<u64>().ok())
        .map_or(false, |pid| pid != 0)
}

fn is_internal_display(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.starts_with("edp") || lower.starts_with("lvds") || lower.starts_with("dsi")
}

pub fn validate_display(
    allowed_displays: i32,
    internal_display_only: bool,
    ignore_error: bool,
    qt_screen_names: &[String],
) -> Result<(), String> {
    if allowed_displays < 1 {
        return Err("The allowed display count must be positive.".to_string());
    }

    let mut count = 0i32;
    let mut external = 0i32;

    if let Ok(entries) = fs::read_dir("/sys/class/drm") {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with("card") || !name.contains('-') {
                continue;
            }
            let base = entry.path();
            let status = fs::read_to_string(base.join("status"))
                .unwrap_or_default()
                .trim()
                .to_string();
            let enabled = fs::read_to_string(base.join("enabled"))
                .unwrap_or_default()
                .trim()
                .to_string();
            if status != "connected" || enabled != "enabled" {
                continue;
            }
            count += 1;
            if let Some(idx) = name.find('-') {
                if !is_internal_display(&name[idx + 1..]) {
                    external += 1;
                }
            }
        }
    }

    let qt_count = qt_screen_names.len() as i32;
    if count == 0 {
        for screen in qt_screen_names {
            count += 1;
            if !is_internal_display(screen) {
                external += 1;
            }
        }
    } else if qt_count > count {
        external += qt_count - count;
        count = qt_count;
    }

    if count == 0 {
        return if ignore_error {
            Ok(())
        } else {
            Err("Unable to validate the display configuration.".to_string())
        };
    }
    if count > allowed_displays {
        return Err(format!(
            "Detected {count} active displays; this exam allows {allowed_displays}."
        ));
    }
    if internal_display_only && external > 0 {
        return Err("This exam permits internal displays only.".to_string());
    }
    Ok(())
}

pub fn detect_prohibited_processes(blacklist: &[String]) -> Vec<String> {
    let mut prohibited = HashSet::new();
    for item in blacklist {
        let name = Path::new(item.trim())
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_lowercase();
        if !name.is_empty() {
            prohibited.insert(name);
        }
    }
    if prohibited.is_empty() {
        return Vec::new();
    }

    let self_pid = unsafe { libc::getpid() as i64 };
    let mut detected = Vec::new();
    let mut seen = HashSet::new();

    let Ok(entries) = fs::read_dir("/proc") else {
        return detected;
    };
    for entry in entries.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<i64>() else {
            continue;
        };
        if pid <= 1 || pid == self_pid {
            continue;
        }
        let exe_name = fs::read_link(format!("/proc/{pid}/exe"))
            .ok()
            .and_then(|exe| {
                let mut path = exe.to_string_lossy().into_owned();
                if path.ends_with(" (deleted)") && !Path::new(&path).exists() {
                    path.truncate(path.len() - " (deleted)".len());
                }
                Path::new(&path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.to_lowercase())
            })
            .unwrap_or_default();

        let comm_name = fs::read_to_string(format!("/proc/{pid}/comm"))
            .map(|s| s.trim().to_lowercase())
            .unwrap_or_default();

        let cmdline_hit = fs::read(format!("/proc/{pid}/cmdline"))
            .ok()
            .and_then(|bytes| {
                bytes
                    .split(|&b| b == 0)
                    .filter(|s| !s.is_empty())
                    .take(4)
                    .filter_map(|s| std::str::from_utf8(s).ok())
                    .filter(|arg| !arg.starts_with('-'))
                    .filter_map(|arg| {
                        Path::new(arg.trim())
                            .file_name()
                            .and_then(|n| n.to_str())
                            .map(|n| n.to_lowercase())
                    })
                    .find(|n| prohibited.contains(n))
            });

        let matched = if !exe_name.is_empty() && prohibited.contains(&exe_name) {
            Some(exe_name)
        } else if !comm_name.is_empty() && prohibited.contains(&comm_name) {
            Some(comm_name)
        } else if comm_name.len() == 15 {
            prohibited
                .iter()
                .find(|p| p.len() > 15 && p.starts_with(&comm_name))
                .cloned()
                .or(cmdline_hit)
        } else {
            cmdline_hit
        };

        if let Some(name) = matched {
            if seen.insert(name.clone()) {
                detected.push(name);
            }
        }
    }
    detected
}

// =========================================================================
// SessionIntegrityStore (atomic SHA-256 checksummed session history + flock)
// =========================================================================

const FILE_MAGIC: &[u8] = b"SEB-RUST-SESSION-HISTORY";
const FILE_VERSION: u16 = 1;
const MAX_RECORDS: usize = 4096;
const MAX_FILE_SIZE: u64 = 4 * 1024 * 1024;

fn is_valid_key(key: &str) -> bool {
    key.len() == 64 && key.bytes().all(|b| b.is_ascii_hexdigit())
}

pub struct SessionIntegrityStore {
    file_path: PathBuf,
    lock_path: PathBuf,
    lock_file: Option<File>,
    active_key: String,
    prepared_key: String,
    prepared_has_quit_password: bool,
    prepared_verify: bool,
    prepared: bool,
}

impl SessionIntegrityStore {
    pub fn new(file_path: impl Into<PathBuf>) -> Self {
        let file_path = file_path.into();
        let mut lock_os = file_path.as_os_str().to_os_string();
        lock_os.push(".lock");
        Self {
            file_path,
            lock_path: PathBuf::from(lock_os),
            lock_file: None,
            active_key: String::new(),
            prepared_key: String::new(),
            prepared_has_quit_password: false,
            prepared_verify: false,
            prepared: false,
        }
    }

    pub fn prepare(
        &mut self,
        configuration_key: &str,
        has_quit_password: bool,
        verify: bool,
    ) -> Result<(), String> {
        self.prepared = false;
        self.prepared_key = configuration_key.to_string();
        self.prepared_has_quit_password = has_quit_password;
        self.prepared_verify = verify;

        if has_quit_password || self.lock_file.is_some() {
            self.acquire()?;
        }

        if verify && has_quit_password {
            if !is_valid_key(configuration_key) {
                return Err("Session verification requires a valid configuration key.".to_string());
            }
            let records = self.read_records()?;
            if records.iter().any(|k| k == configuration_key) && configuration_key != self.active_key
            {
                return Err(
                    "This configuration was found in unfinished session history. The previous exam session did not shut down cleanly."
                        .to_string(),
                );
            }
        }

        self.prepared = true;
        Ok(())
    }

    pub fn activate(
        &mut self,
        configuration_key: &str,
        has_quit_password: bool,
        verify: bool,
    ) -> Result<(), String> {
        if !self.prepared
            || self.prepared_key != configuration_key
            || self.prepared_has_quit_password != has_quit_password
            || self.prepared_verify != verify
        {
            return Err("Session integrity was not prepared for this configuration.".to_string());
        }

        if !has_quit_password && self.lock_file.is_none() {
            self.prepared = false;
            return Ok(());
        }

        self.acquire()?;
        let mut records = self.read_records()?;
        if verify
            && has_quit_password
            && records.iter().any(|k| k == configuration_key)
            && configuration_key != self.active_key
        {
            return Err(
                "This configuration was found in unfinished session history. The previous exam session did not shut down cleanly."
                    .to_string(),
            );
        }

        if !self.active_key.is_empty() {
            records.retain(|k| k != &self.active_key);
        }
        if verify && has_quit_password && !records.iter().any(|k| k == configuration_key) {
            records.push(configuration_key.to_string());
        }
        self.write_records(&records)?;

        self.active_key = if has_quit_password {
            configuration_key.to_string()
        } else {
            String::new()
        };
        self.prepared = false;
        if self.active_key.is_empty() {
            self.release();
        }
        Ok(())
    }

    pub fn finish(&mut self, clean_shutdown: bool) -> Result<(), String> {
        if self.lock_file.is_none() {
            return Ok(());
        }
        if !clean_shutdown {
            self.release();
            return Ok(());
        }

        let mut records = match self.read_records() {
            Ok(r) => r,
            Err(e) => {
                self.release();
                return Err(e);
            }
        };
        if !self.active_key.is_empty() {
            records.retain(|k| k != &self.active_key);
        }
        let res = self.write_records(&records);
        self.active_key.clear();
        self.prepared = false;
        self.release();
        res
    }

    fn acquire(&mut self) -> Result<(), String> {
        if self.lock_file.is_some() {
            return Ok(());
        }
        if let Some(parent) = self.file_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|_| "Could not create the session history directory.".to_string())?;
            let meta = fs::symlink_metadata(parent)
                .map_err(|_| "Could not inspect the session history directory.".to_string())?;
            if meta.file_type().is_symlink() {
                return Err("Could not secure the session history directory permissions.".to_string());
            }
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
                .map_err(|_| "Could not secure the session history directory permissions.".to_string())?;
        }

        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&self.lock_path)
            .map_err(|_| "Another Safe Exam Browser session is using session integrity storage.".to_string())?;
        use std::os::fd::AsRawFd;
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc != 0 {
            return Err(
                "Another Safe Exam Browser session is using session integrity storage.".to_string(),
            );
        }
        self.lock_file = Some(file);
        Ok(())
    }

    fn read_records(&self) -> Result<Vec<String>, String> {
        if !self.file_path.exists() {
            return Ok(Vec::new());
        }
        let meta = fs::symlink_metadata(&self.file_path)
            .map_err(|_| "Could not read protected session history.".to_string())?;
        let euid = unsafe { libc::geteuid() };
        if meta.file_type().is_symlink() || meta.uid() != euid || (meta.mode() & 0o077) != 0 {
            return Err(
                "Protected session history has unsafe ownership or permissions.".to_string(),
            );
        }
        if meta.len() == 0 || meta.len() > MAX_FILE_SIZE {
            return Err("Protected session history has an invalid size.".to_string());
        }

        let mut bytes = Vec::new();
        File::open(&self.file_path)
            .and_then(|mut f| f.read_to_end(&mut bytes))
            .map_err(|_| "Could not read protected session history.".to_string())?;

        // Format: MAGIC (24) | VERSION (2 LE) | PAYLOAD_LEN (4 LE) | PAYLOAD | SHA256 (32)
        let header_len = FILE_MAGIC.len() + 2 + 4;
        if bytes.len() < header_len + 32 || &bytes[..FILE_MAGIC.len()] != FILE_MAGIC {
            // If legacy Qt QDataStream cache file exists from previous C++ build, reject cleanly
            // only when corrupted; otherwise verify strictly.
            return Err("Protected session history is corrupted or unsupported.".to_string());
        }
        let version = u16::from_le_bytes([bytes[FILE_MAGIC.len()], bytes[FILE_MAGIC.len() + 1]]);
        if version != FILE_VERSION {
            return Err("Protected session history is corrupted or unsupported.".to_string());
        }
        let payload_len = u32::from_le_bytes([
            bytes[FILE_MAGIC.len() + 2],
            bytes[FILE_MAGIC.len() + 3],
            bytes[FILE_MAGIC.len() + 4],
            bytes[FILE_MAGIC.len() + 5],
        ]) as usize;
        if bytes.len() != header_len + payload_len + 32 {
            return Err("Protected session history is corrupted or unsupported.".to_string());
        }
        let payload = &bytes[header_len..header_len + payload_len];
        let checksum = &bytes[header_len + payload_len..];
        let computed = Sha256::digest(payload);
        if computed.as_slice() != checksum {
            return Err("Protected session history is corrupted or unsupported.".to_string());
        }

        let text = std::str::from_utf8(payload)
            .map_err(|_| "Protected session history contains invalid records.".to_string())?;
        let mut records = Vec::new();
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if !is_valid_key(trimmed) {
                return Err(
                    "Protected session history contains an invalid configuration key.".to_string(),
                );
            }
            if !records.iter().any(|k| k == trimmed) {
                records.push(trimmed.to_string());
            }
        }
        if records.len() > MAX_RECORDS {
            return Err("Protected session history contains invalid records.".to_string());
        }
        Ok(records)
    }

    fn write_records(&self, records: &[String]) -> Result<(), String> {
        if records.len() > MAX_RECORDS {
            return Err("Protected session history exceeds its record limit.".to_string());
        }
        for key in records {
            if !is_valid_key(key) {
                return Err(
                    "Refusing to write an invalid configuration key to session history."
                        .to_string(),
                );
            }
        }

        let payload = records.join("\n").into_bytes();
        let checksum = Sha256::digest(&payload);

        let mut out = Vec::with_capacity(FILE_MAGIC.len() + 6 + payload.len() + 32);
        out.extend_from_slice(FILE_MAGIC);
        out.extend_from_slice(&FILE_VERSION.to_le_bytes());
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&payload);
        out.extend_from_slice(&checksum);

        let mut tmp_os = self.file_path.as_os_str().to_os_string();
        tmp_os.push(".tmp");
        let tmp_path = PathBuf::from(tmp_os);
        let mut f = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&tmp_path)
            .map_err(|_| "Could not open protected session history for writing.".to_string())?;
        fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o600))
            .map_err(|_| "Could not secure protected session history permissions.".to_string())?;
        f.write_all(&out)
            .and_then(|_| f.sync_all())
            .map_err(|_| "Could not safely update protected session history.".to_string())?;
        drop(f);
        fs::rename(&tmp_path, &self.file_path)
            .map_err(|_| "Could not safely update protected session history.".to_string())?;
        Ok(())
    }

    fn release(&mut self) {
        if let Some(f) = self.lock_file.take() {
            use std::os::fd::AsRawFd;
            unsafe {
                libc::flock(f.as_raw_fd(), libc::LOCK_UN);
            }
        }
    }
}

impl Drop for SessionIntegrityStore {
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_restriction_min_and_exact() {
        let min_req = vec![VersionRestriction {
            major: 3,
            minor: 7,
            patch: Some(0),
            build: None,
            is_minimum_restriction: true,
            requires_alliance_edition: false,
        }];
        assert!(validate_version_restrictions(&min_req, "3.10.2.0", false));
        assert!(!validate_version_restrictions(&min_req, "3.6.0.0", false));
    }

    #[test]
    fn session_integrity_detects_dirty_shutdown_and_clears_on_clean() {
        let dir = std::env::temp_dir().join(format!("seb-rust-sec-test-{}", unsafe {
            libc::getpid()
        }));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("cache.bin");
        let key = "a".repeat(64);

        {
            let mut store = SessionIntegrityStore::new(&path);
            store.prepare(&key, true, true).unwrap();
            store.activate(&key, true, true).unwrap();
            store.finish(false).unwrap();
        }
        {
            let mut store = SessionIntegrityStore::new(&path);
            assert!(store.prepare(&key, true, true).is_err());
        }
        let _ = fs::remove_file(&path);
        {
            let mut store = SessionIntegrityStore::new(&path);
            store.prepare(&key, true, true).unwrap();
            store.activate(&key, true, true).unwrap();
            store.finish(true).unwrap();
        }
        {
            let mut store = SessionIntegrityStore::new(&path);
            store.prepare(&key, true, true).unwrap();
            store.finish(true).unwrap();
        }
        let _ = fs::remove_dir_all(&dir);
    }
}
