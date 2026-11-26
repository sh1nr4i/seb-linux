use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

fn read_trimmed(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

fn read_cmdline_args(path: &Path) -> Vec<String> {
    let Ok(bytes) = fs::read(path) else {
        return Vec::new();
    };
    bytes
        .split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .filter_map(|s| std::str::from_utf8(s).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn basename_lower(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    Path::new(trimmed)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(trimmed)
        .to_lowercase()
}

fn is_wrapper_executable(exe_lower: &str) -> bool {
    exe_lower.starts_with("python")
        || exe_lower.starts_with("electron")
        || matches!(
            exe_lower,
            "sh" | "bash" | "dash" | "zsh" | "env" | "node" | "nodejs" | "gjs" | "flatpak-spawn"
        )
}

/// Checks whether a process's `exe` basename, 15-char `comm`, or `cmdline` argv matches any
/// entry in the prohibited executable set.
pub fn match_prohibited_name(
    prohibited: &HashSet<String>,
    exe_name: &str,
    comm_name: &str,
    cmdline_args: &[String],
) -> Option<String> {
    if !exe_name.is_empty() && prohibited.contains(exe_name) {
        return Some(exe_name.to_string());
    }
    if !comm_name.is_empty() {
        if prohibited.contains(comm_name) {
            return Some(comm_name.to_string());
        }
        // Linux kernel TASK_COMM_LEN truncates /proc/<pid>/comm at 15 chars (e.g. simplescreenrec).
        if comm_name.len() == 15 {
            if let Some(full) = prohibited
                .iter()
                .find(|p| p.len() > 15 && p.starts_with(comm_name))
            {
                return Some(full.clone());
            }
        }
    }
    if let Some(arg0) = cmdline_args.first() {
        let b0 = basename_lower(arg0);
        if !b0.is_empty() && prohibited.contains(&b0) {
            return Some(b0);
        }
    }
    let check_wrapped = exe_name.is_empty()
        || is_wrapper_executable(exe_name)
        || is_wrapper_executable(comm_name);
    if check_wrapped {
        for arg in cmdline_args
            .iter()
            .skip(1)
            .filter(|a| !a.starts_with('-'))
            .take(4)
        {
            let b = basename_lower(arg);
            if !b.is_empty() && prohibited.contains(&b) {
                return Some(b);
            }
        }
    }
    None
}

fn read_parent_pid(pid: i64) -> i64 {
    let stat_path = format!("/proc/{pid}/stat");
    let Some(text) = read_trimmed(Path::new(&stat_path)) else {
        return 0;
    };
    let Some(close_paren) = text.rfind(')') else {
        return 0;
    };
    let rest = text.get(close_paren + 2..).unwrap_or("");
    let mut fields = rest.split_whitespace();
    let _state = fields.next();
    fields
        .next()
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0)
}

fn is_descendant_of(pid: i64, ancestor: i64, cache: &mut HashMap<i64, i64>) -> bool {
    if ancestor <= 0 || pid <= 0 {
        return false;
    }
    let mut current = pid;
    for _ in 0..32 {
        if current <= 1 {
            break;
        }
        if current == ancestor {
            return true;
        }
        let parent = *cache.entry(current).or_insert_with(|| read_parent_pid(current));
        if parent == current || parent <= 1 {
            break;
        }
        current = parent;
    }
    false
}

pub fn scan_prohibited_processes(blacklist: &[String], seb_pid: i64) -> Vec<String> {
    let mut prohibited = HashSet::new();
    for entry in blacklist {
        let name = basename_lower(entry);
        if !name.is_empty() {
            prohibited.insert(name);
        }
    }
    if prohibited.is_empty() {
        return Vec::new();
    }

    let self_pid = unsafe { libc::getpid() as i64 };
    let direct_ppid = unsafe { libc::getppid() as i64 };
    let mut ppid_cache: HashMap<i64, i64> = HashMap::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut detected: Vec<String> = Vec::new();

    let Ok(entries) = fs::read_dir("/proc") else {
        return detected;
    };

    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(pid_str) = file_name.to_str() else {
            continue;
        };
        let Ok(pid) = pid_str.parse::<i64>() else {
            continue;
        };
        if pid <= 1 || pid == self_pid || pid == direct_ppid || pid == seb_pid {
            continue;
        }
        if seb_pid > 1 && is_descendant_of(pid, seb_pid, &mut ppid_cache) {
            continue;
        }

        let exe_link = format!("/proc/{pid}/exe");
        let exe_name = fs::read_link(&exe_link)
            .ok()
            .map(|p| {
                let mut s = p.to_string_lossy().into_owned();
                if s.ends_with(" (deleted)") && !Path::new(&s).exists() {
                    s.truncate(s.len() - " (deleted)".len());
                }
                basename_lower(&s)
            })
            .unwrap_or_default();

        let comm_path = format!("/proc/{pid}/comm");
        let comm_name = read_trimmed(Path::new(&comm_path))
            .map(|s| s.to_lowercase())
            .unwrap_or_default();

        let cmdline_args = read_cmdline_args(Path::new(&format!("/proc/{pid}/cmdline")));

        if let Some(name) = match_prohibited_name(&prohibited, &exe_name, &comm_name, &cmdline_args)
        {
            let label = format!("{name} (PID {pid})");
            if seen.insert(label.clone()) {
                detected.push(label);
            }
        }
    }

    detected.sort_by_key(|s| s.to_lowercase());
    detected
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_blacklist_returns_empty() {
        assert!(scan_prohibited_processes(&[], 0).is_empty());
    }

    #[test]
    fn matches_truncated_comm_and_script_wrappers() {
        let mut set = HashSet::new();
        set.insert("simplescreenrecorder".to_string());
        set.insert("kazam".to_string());
        set.insert("obs".to_string());

        // 15-char kernel TASK_COMM_LEN truncation match
        assert_eq!(
            match_prohibited_name(&set, "", "simplescreenrec", &[]),
            Some("simplescreenrecorder".to_string())
        );

        // Scripted/interpreted recorder launched via python3
        assert_eq!(
            match_prohibited_name(
                &set,
                "python3.12",
                "python3",
                &["/usr/bin/python3".to_string(), "/usr/bin/kazam".to_string()],
            ),
            Some("kazam".to_string())
        );

        // Wrapper launched with 5+ leading flags must still detect the target script/binary
        assert_eq!(
            match_prohibited_name(
                &set,
                "python3.12",
                "python3",
                &[
                    "/usr/bin/python3".to_string(),
                    "-u".to_string(),
                    "-O".to_string(),
                    "-B".to_string(),
                    "-s".to_string(),
                    "-E".to_string(),
                    "/usr/bin/kazam".to_string(),
                ],
            ),
            Some("kazam".to_string())
        );
    }
}
