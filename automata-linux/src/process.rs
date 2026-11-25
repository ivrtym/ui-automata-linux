use std::fs;
use std::path::Path;
use std::process::Command;

use crate::{Result, UiError};

fn basename_clean(raw: &str, strip_deleted: bool) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let clean = if strip_deleted {
        trimmed.strip_suffix(" (deleted)").unwrap_or(trimmed)
    } else {
        trimmed
    };
    let base = Path::new(clean)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(clean)
        .trim();
    if base.is_empty() {
        None
    } else {
        Some(base.to_string())
    }
}

/// Resolve the cleanest untruncated process name from `/proc/<pid>` inputs.
///
/// Linux limits `/proc/<pid>/comm` to 15 bytes (`TASK_COMM_LEN - 1`), which truncates
/// names like `gnome-calculator` to `gnome-calculato`. When `comm` is shorter than
/// 15 bytes, it is returned directly (preserving launcher names such as `gjs` or
/// `python3`). When `comm` hits the 15-byte boundary (or is missing), untruncated
/// `argv[0]` / `/proc/<pid>/exe` basenames that extend `comm` are preferred.
fn resolve_process_name(
    comm: Option<&str>,
    cmdline_argv0: Option<&str>,
    exe_target: Option<&str>,
) -> Option<String> {
    let comm_trimmed = comm.map(str::trim).filter(|s| !s.is_empty());
    let argv0_base = cmdline_argv0.and_then(|s| basename_clean(s, false));
    let exe_base = exe_target.and_then(|s| basename_clean(s, true));

    match comm_trimmed {
        Some(c) if c.len() < 15 => Some(c.to_string()),
        Some(c) => {
            let extends = |candidate: &str| {
                candidate.len() > c.len()
                    && candidate.is_char_boundary(c.len())
                    && candidate[..c.len()].eq_ignore_ascii_case(c)
            };
            if let Some(a0) = argv0_base.as_deref().filter(|s| extends(s)) {
                return Some(a0.to_string());
            }
            if let Some(ex) = exe_base.as_deref().filter(|s| extends(s)) {
                return Some(ex.to_string());
            }
            Some(c.to_string())
        }
        None => exe_base.or(argv0_base),
    }
}

/// Read the untruncated process name (without directory) for `pid` from `/proc/<pid>`.
pub fn get_process_name(pid: i32) -> Result<String> {
    if pid <= 0 {
        return Err(UiError::Platform(format!("invalid pid {pid}")).into());
    }
    let comm = fs::read_to_string(format!("/proc/{pid}/comm")).ok();
    let argv0 = if comm.as_deref().map(|s| s.trim().len() >= 15).unwrap_or(true) {
        fs::read(format!("/proc/{pid}/cmdline"))
            .ok()
            .and_then(|bytes| {
                let first = bytes.split(|b| *b == 0).next()?;
                if first.is_empty() {
                    None
                } else {
                    Some(String::from_utf8_lossy(first).into_owned())
                }
            })
    } else {
        None
    };
    let exe = if comm.as_deref().map(|s| s.trim().len() >= 15).unwrap_or(true) {
        fs::read_link(format!("/proc/{pid}/exe"))
            .ok()
            .map(|p| p.to_string_lossy().into_owned())
    } else {
        None
    };

    if let Some(name) = resolve_process_name(comm.as_deref(), argv0.as_deref(), exe.as_deref()) {
        return Ok(name);
    }
    Err(UiError::Platform(format!("process {pid} not found in /proc")).into())
}

/// Enumerate all running `(pid, name)` pairs from `/proc`.
pub fn list_processes() -> Result<Vec<(u32, String)>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(Path::new("/proc"))? {
        let Ok(entry) = entry else { continue };
        let fname = entry.file_name();
        let Some(s) = fname.to_str() else { continue };
        let Ok(pid) = s.parse::<u32>() else { continue };
        if let Ok(name) = get_process_name(pid as i32) {
            out.push((pid, name));
        }
    }
    out.sort_by_key(|(pid, _)| *pid);
    Ok(out)
}

/// Send `SIGTERM` to a process by PID.
pub fn kill_process_by_pid(pid: u32) -> Result<()> {
    let status = Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()?;
    if !status.success() {
        return Err(UiError::Platform(format!("kill -TERM {pid} exited with {status}")).into());
    }
    Ok(())
}

/// Send `SIGTERM` to all processes whose name matches `name` (case-insensitive).
pub fn kill_processes_by_name(name: &str) -> Result<Vec<u32>> {
    let target = name.trim();
    if target.is_empty() {
        return Err(UiError::Internal("process_name must not be empty".into()).into());
    }
    let mut killed = Vec::new();
    for (pid, proc_name) in list_processes()? {
        if ui_automata::process_names_match(&proc_name, target) {
            kill_process_by_pid(pid)?;
            killed.push(pid);
        }
    }
    if killed.is_empty() {
        return Err(UiError::Internal(format!("no running process named '{target}'")).into());
    }
    Ok(killed)
}

#[cfg(test)]
mod tests {
    use super::resolve_process_name;

    #[test]
    fn recovers_full_name_when_linux_comm_hits_15_byte_limit() {
        assert_eq!(
            resolve_process_name(
                Some("gnome-calculato\n"),
                Some("/usr/bin/gnome-calculator"),
                Some("/usr/bin/gnome-calculator"),
            ),
            Some("gnome-calculator".into())
        );
        assert_eq!(
            resolve_process_name(
                Some("gnome-calculato"),
                None,
                Some("/usr/bin/gnome-calculator (deleted)"),
            ),
            Some("gnome-calculator".into())
        );
    }

    #[test]
    fn preserves_short_interpreter_and_launcher_comm_names() {
        assert_eq!(
            resolve_process_name(Some("gjs\n"), Some("gjs"), Some("/usr/bin/gjs-console")),
            Some("gjs".into())
        );
        assert_eq!(
            resolve_process_name(
                Some("python3\n"),
                Some("python3"),
                Some("/usr/bin/python3.13"),
            ),
            Some("python3".into())
        );
    }
}
