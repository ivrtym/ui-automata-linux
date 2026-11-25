//! Accessibility text normalization, Chromium noise filtering, and private
//! on-disk spill helpers for oversized UI trees.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

/// Unicode Object Replacement Character (`U+FFFC`) emitted by Chromium / GTK / Qt
/// inline embedded-object spans without meaningful accessible text.
pub const OBJECT_REPLACEMENT: char = '\u{fffc}';

/// Inline JSON byte budget (~10k tokens ceiling) before spilling full tree JSON
/// to a private per-user temporary file.
pub const INLINE_TREE_RESULT_BYTES: usize = 32_000;

const MAX_SPILL_AGE: Duration = Duration::from_secs(60 * 60);
const SPILL_PREFIX: &str = "tree-";

static SPILL_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Returns `true` when `text` consists exclusively of whitespace and Unicode
/// object replacement placeholders (`U+FFFC`).
pub fn is_placeholder_text(text: &str) -> bool {
    text.chars()
        .all(|ch| ch == OBJECT_REPLACEMENT || ch.is_whitespace())
}

/// Strips `U+FFFC` placeholder characters and outer whitespace when the string
/// is empty/placeholder-only.
pub fn strip_placeholder_text(text: &str) -> String {
    if !text.contains(OBJECT_REPLACEMENT) {
        if text.trim().is_empty() {
            return String::new();
        }
        return text.to_string();
    }
    let cleaned: String = text
        .chars()
        .filter(|&ch| ch != OBJECT_REPLACEMENT)
        .collect();
    if cleaned.trim().is_empty() {
        String::new()
    } else {
        cleaned.trim().to_string()
    }
}

/// Normalizes an optional text property, turning empty or `U+FFFC`-only values
/// into `None`.
pub fn clean_optional_text(text: Option<String>) -> Option<String> {
    let raw = text?;
    let cleaned = strip_placeholder_text(&raw);
    (!cleaned.is_empty()).then_some(cleaned)
}

/// Chromium exposes synthetic default actions (`doDefault`, `showContextMenu`,
/// `clickAncestor`) on nearly every layout container.
pub fn is_chromium_default_action(name: &str) -> bool {
    matches!(name, "doDefault" | "showContextMenu" | "clickAncestor")
}

/// Metadata returned when an element tree response exceeds `INLINE_TREE_RESULT_BYTES`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeSpillInfo {
    pub path: String,
    pub bytes: usize,
}

/// Default private per-user directory for spilled tree payloads (`0700`).
pub fn default_spill_dir() -> PathBuf {
    let uid = unsafe { libc_euid_fallback() };
    if let Ok(runtime) = std::env::var("XDG_RUNTIME_DIR") {
        let trimmed = runtime.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed).join("automata-linux").join("trees");
        }
    }
    std::env::temp_dir().join(format!("automata-linux-trees-{uid}"))
}

#[cfg(unix)]
unsafe fn libc_euid_fallback() -> u32 {
    // Parse /proc/self/status Uid without pulling extra FFI crates.
    if let Ok(status) = fs::read_to_string("/proc/self/status") {
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("Uid:") {
                if let Some(euid) = rest.split_whitespace().nth(1).and_then(|v| v.parse().ok()) {
                    return euid;
                }
            }
        }
    }
    0
}

/// Ensures `dir` exists as a private (`0700`) directory owned by this user,
/// sweeps stale spill files, and writes `bytes` into a private (`0600`) file.
pub fn write_private_spill_file(
    bytes: &[u8],
    dir_override: Option<&Path>,
) -> std::io::Result<TreeSpillInfo> {
    let dir = dir_override.map(Path::to_path_buf).unwrap_or_else(default_spill_dir);
    ensure_private_dir(&dir)?;
    sweep_stale_spill_files(&dir);

    let pid = std::process::id();
    for _ in 0..8 {
        let seq = SPILL_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = dir.join(format!("{SPILL_PREFIX}{pid}-{seq}.json"));
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true).mode(0o600);
        match opts.open(&path) {
            Ok(mut file) => {
                file.write_all(bytes)?;
                file.flush()?;
                return Ok(TreeSpillInfo {
                    path: path.display().to_string(),
                    bytes: bytes.len(),
                });
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "failed to allocate unique spill filename",
    ))
}

fn ensure_private_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder.create(dir)?;
    let meta = fs::symlink_metadata(dir)?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "spill directory must be a real directory, not a symlink",
        ));
    }
    let self_uid = fs::metadata("/proc/self")?.uid();
    if meta.uid() != self_uid {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "spill directory is not owned by the current user",
        ));
    }
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
}

fn sweep_stale_spill_files(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.starts_with(SPILL_PREFIX) || !name.ends_with(".json") {
            continue;
        }
        let stale_by_age = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|mtime| now.duration_since(mtime).ok())
            .is_some_and(|age| age > MAX_SPILL_AGE);

        let dead_pid = parse_spill_pid(name)
            .is_some_and(|pid| pid != std::process::id() && !Path::new(&format!("/proc/{pid}")).exists());

        if stale_by_age || dead_pid {
            let _ = fs::remove_file(path);
        }
    }
}

fn parse_spill_pid(file_name: &str) -> Option<u32> {
    let rest = file_name.strip_prefix(SPILL_PREFIX)?;
    let pid_part = rest.split('-').next()?;
    pid_part.parse::<u32>().ok()
}

/// If `payload` serializes past `inline_budget` bytes, writes the full JSON to a
/// private spill file and returns a compact summary envelope preserving inline
/// metadata and a bounded preview; otherwise returns `payload` unchanged.
pub fn spill_json_if_oversized(
    payload: serde_json::Value,
    inline_budget: usize,
    dir_override: Option<&Path>,
) -> serde_json::Value {
    let Ok(serialized) = serde_json::to_vec(&payload) else {
        return payload;
    };
    if serialized.len() <= inline_budget {
        return payload;
    }

    match write_private_spill_file(&serialized, dir_override) {
        Ok(spill) => {
            let mut summary = match payload {
                serde_json::Value::Object(mut map) => {
                    // Replace heavy array/tree bodies with a shallow truncated preview
                    if let Some(serde_json::Value::Array(arr)) = map.get("windows") {
                        let preview: Vec<_> = arr.iter().take(2).cloned().collect();
                        map.insert("windows_preview".to_string(), serde_json::Value::Array(preview));
                        map.remove("windows");
                    }
                    if let Some(tree) = map.remove("tree") {
                        if let Some(obj) = tree.as_object() {
                            let mut shallow = serde_json::Map::new();
                            for k in ["name", "role", "automation_id", "hwnd", "bounds", "truncated"] {
                                if let Some(v) = obj.get(k) {
                                    shallow.insert(k.to_string(), v.clone());
                                }
                            }
                            map.insert("tree_summary".to_string(), serde_json::Value::Object(shallow));
                        }
                    }
                    map
                }
                _ => serde_json::Map::new(),
            };
            summary.insert("truncated_inline".to_string(), serde_json::Value::Bool(true));
            summary.insert("spill_path".to_string(), serde_json::Value::String(spill.path));
            summary.insert("spill_bytes".to_string(), serde_json::json!(spill.bytes));
            summary.insert("inline_budget_bytes".to_string(), serde_json::json!(inline_budget));
            serde_json::Value::Object(summary)
        }
        Err(err) => {
            log::warn!("failed to spill oversized element tree JSON: {err}");
            payload
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_object_replacement_placeholders() {
        assert!(is_placeholder_text(""));
        assert!(is_placeholder_text("\u{fffc} \n\t\u{fffc}"));
        assert!(!is_placeholder_text("\u{fffc}Save"));
        assert_eq!(clean_optional_text(Some("\u{fffc}\u{fffc}".into())), None);
        assert_eq!(
            clean_optional_text(Some(" \u{fffc}Submit ".into())),
            Some("Submit".into())
        );
    }

    #[test]
    fn identifies_chromium_synthetic_actions() {
        assert!(is_chromium_default_action("doDefault"));
        assert!(is_chromium_default_action("showContextMenu"));
        assert!(is_chromium_default_action("clickAncestor"));
        assert!(!is_chromium_default_action("press"));
    }

    #[test]
    fn spills_oversized_payload_to_private_dir() {
        let tmp = std::env::temp_dir().join(format!(
            "automata-linux-spill-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&tmp);
        let big = serde_json::json!({
            "count": 1,
            "windows": [{
                "name": "Huge",
                "blob": "x".repeat(4096)
            }]
        });
        let out = spill_json_if_oversized(big.clone(), 256, Some(&tmp));
        assert_eq!(out["truncated_inline"], true);
        let spill_path = out["spill_path"].as_str().expect("spill_path");
        let meta = fs::metadata(spill_path).expect("stat spill file");
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        let dir_meta = fs::metadata(&tmp).expect("stat spill dir");
        assert_eq!(dir_meta.permissions().mode() & 0o777, 0o700);
        let restored: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(spill_path).expect("read spill"))
                .expect("valid json");
        assert_eq!(restored, big);
        let _ = fs::remove_dir_all(&tmp);
    }
}
