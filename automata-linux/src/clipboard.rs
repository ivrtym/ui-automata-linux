use std::io::Write;
use std::process::{Command, Stdio};

use crate::backend::{ClipboardBackend, resolve_clipboard_backend};
use crate::{Result, UiError};

/// Read plain text from the Linux clipboard.
///
/// Auto-detects `wl-clipboard` (Wayland) vs `xclip`/`xsel` (X11),
/// or honors `AUTOMATA_CLIPBOARD_BACKEND=wl-clipboard|xclip|xsel|auto`.
pub fn clipboard_read() -> Result<String> {
    let backend = resolve_clipboard_backend()?;
    match backend {
        ClipboardBackend::WlClipboard => {
            let out = Command::new("wl-paste").arg("--no-newline").output()?;
            if out.status.success() {
                return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
            }
            let stderr = String::from_utf8_lossy(&out.stderr);
            if stderr.contains("Nothing is copied") {
                return Ok(String::new());
            }
            Err(UiError::Platform(format!("wl-paste failed: {}", stderr.trim())).into())
        }
        ClipboardBackend::Xclip => {
            let out = Command::new("xclip")
                .args(["-o", "-selection", "clipboard"])
                .output()?;
            if out.status.success() {
                return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
            }
            let stderr = String::from_utf8_lossy(&out.stderr);
            if stderr.contains("target STRING not available") || stderr.is_empty() {
                return Ok(String::new());
            }
            Err(UiError::Platform(format!("xclip read failed: {}", stderr.trim())).into())
        }
        ClipboardBackend::Xsel => {
            let out = Command::new("xsel").args(["-ob"]).output()?;
            if out.status.success() {
                return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
            }
            let stderr = String::from_utf8_lossy(&out.stderr);
            Err(UiError::Platform(format!("xsel read failed: {}", stderr.trim())).into())
        }
    }
}

/// Write plain text to the Linux clipboard.
///
/// Auto-detects `wl-clipboard` (Wayland) vs `xclip`/`xsel` (X11),
/// or honors `AUTOMATA_CLIPBOARD_BACKEND=wl-clipboard|xclip|xsel|auto`.
pub fn clipboard_write(text: &str) -> Result<()> {
    let backend = resolve_clipboard_backend()?;
    let (cmd, args): (&str, &[&str]) = match backend {
        ClipboardBackend::WlClipboard => ("wl-copy", &[]),
        ClipboardBackend::Xclip => ("xclip", &["-i", "-selection", "clipboard"]),
        ClipboardBackend::Xsel => ("xsel", &["-ib"]),
    };

    let mut child = Command::new(cmd)
        .args(args)
        .stdin(Stdio::piped())
        // wl-copy and xclip daemonize a clipboard-owner child that inherits stdout/stderr;
        // piping stderr and calling wait_with_output() blocks forever waiting for daemon EOF.
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(text.as_bytes())?;
    }
    let status = child.wait()?;
    if status.success() {
        return Ok(());
    }
    Err(UiError::Platform(format!("{cmd} write failed ({status})")).into())
}
