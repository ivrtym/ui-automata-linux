//! Policy-enforced local filesystem operations shared by MCP, CLI, and Rust callers.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::ServiceError;

fn req_str(args: &Value, key: &str) -> Result<String, ServiceError> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| ServiceError::InvalidInput(format!("missing required string parameter '{key}'")))
}

fn opt_str(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(|v| v.as_str()).map(str::to_string)
}

fn opt_u64(args: &Value, key: &str) -> Option<u64> {
    args.get(key).and_then(|v| v.as_u64())
}

fn opt_bool(args: &Value, key: &str, default: bool) -> bool {
    args.get(key).and_then(|v| v.as_bool()).unwrap_or(default)
}

/// Executes a `file` command payload respecting the active `allow_writes` security policy.
pub fn execute_file_request(args: Value, allow_writes: bool) -> Result<Value, ServiceError> {
    let action = req_str(&args, "action")?;
    if !allow_writes
        && matches!(
            action.as_str(),
            "write" | "append" | "copy" | "move" | "delete" | "mkdir" | "rmdir"
        )
    {
        return Err(ServiceError::PermissionDenied(format!(
            "file mutation '{action}' is disabled by current file write policy"
        )));
    }

    match action.as_str() {
        "list" => {
            let path = req_str(&args, "path")?;
            let mut entries = Vec::new();
            for entry in std::fs::read_dir(&path)? {
                let entry = entry?;
                let meta = entry.metadata().ok();
                entries.push(json!({
                    "name": entry.file_name().to_string_lossy().into_owned(),
                    "path": entry.path().display().to_string(),
                    "is_dir": meta.as_ref().is_some_and(|m| m.is_dir()),
                    "size": meta.as_ref().map(|m| m.len()),
                }));
            }
            Ok(Value::Array(entries))
        }
        "read" => {
            let path = req_str(&args, "path")?;
            let bytes = std::fs::read(&path)?;
            match String::from_utf8(bytes) {
                Ok(text) => Ok(json!({ "encoding": "utf-8", "content": text })),
                Err(e) => {
                    let b64 = base64::engine::general_purpose::STANDARD.encode(e.into_bytes());
                    Ok(json!({ "encoding": "base64", "content": b64 }))
                }
            }
        }
        "read_lines" => {
            let path = req_str(&args, "path")?;
            let offset = args.get("offset").and_then(|v| v.as_i64()).unwrap_or(0);
            let count = opt_u64(&args, "count").unwrap_or(100) as usize;
            let content = std::fs::read_to_string(&path)?;
            let all_lines: Vec<&str> = content.lines().collect();
            let total = all_lines.len();
            let start = if offset < 0 {
                total.saturating_sub(offset.unsigned_abs() as usize)
            } else {
                (offset as usize).min(total)
            };
            let end = (start.saturating_add(count)).min(total);
            Ok(json!({
                "total_lines": total,
                "offset": start,
                "lines": all_lines[start..end],
            }))
        }
        "write" => {
            let path = req_str(&args, "path")?;
            let content = req_str(&args, "content")?;
            let overwrite = opt_bool(&args, "overwrite", false);
            let encoding = opt_str(&args, "encoding").unwrap_or_else(|| "utf-8".into());
            let target = Path::new(&path);
            if target.exists() && !overwrite {
                return Err(ServiceError::Conflict(format!(
                    "file '{}' exists and overwrite is false",
                    target.display()
                )));
            }
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            if encoding == "base64" {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(content)
                    .map_err(|e| ServiceError::InvalidInput(format!("invalid base64 content: {e}")))?;
                std::fs::write(target, bytes)?;
            } else {
                std::fs::write(target, content)?;
            }
            Ok(json!({ "ok": true, "path": path }))
        }
        "append" => {
            let path = req_str(&args, "path")?;
            let content = req_str(&args, "content")?;
            let mut f = OpenOptions::new().create(true).append(true).open(&path)?;
            f.write_all(content.as_bytes())?;
            Ok(json!({ "ok": true, "path": path }))
        }
        "copy" => {
            let src = req_str(&args, "src")?;
            let dst = req_str(&args, "dst")?;
            let bytes = std::fs::copy(&src, &dst)?;
            Ok(json!({ "ok": true, "bytes": bytes }))
        }
        "move" => {
            let src = req_str(&args, "src")?;
            let dst = req_str(&args, "dst")?;
            std::fs::rename(&src, &dst)?;
            Ok(json!({ "ok": true }))
        }
        "delete" => {
            let path = req_str(&args, "path")?;
            std::fs::remove_file(&path)?;
            Ok(json!({ "ok": true }))
        }
        "mkdir" => {
            let path = req_str(&args, "path")?;
            std::fs::create_dir_all(&path)?;
            Ok(json!({ "ok": true }))
        }
        "rmdir" => {
            let path = req_str(&args, "path")?;
            std::fs::remove_dir_all(&path)?;
            Ok(json!({ "ok": true }))
        }
        "stat" => {
            let path = req_str(&args, "path")?;
            let meta = std::fs::metadata(&path)?;
            let modified = meta
                .modified()
                .ok()
                .map(|t| chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339());
            Ok(json!({
                "exists": true,
                "is_file": meta.is_file(),
                "is_dir": meta.is_dir(),
                "size": meta.len(),
                "readonly": meta.permissions().readonly(),
                "modified": modified,
            }))
        }
        "checksum" => {
            let path = req_str(&args, "path")?;
            let bytes = std::fs::read(&path)?;
            let digest = Sha256::digest(&bytes);
            Ok(json!({ "algorithm": "sha256", "checksum": hex::encode(digest) }))
        }
        "glob" => {
            let pattern = req_str(&args, "pattern")?;
            let full_pattern = if let Some(root) = opt_str(&args, "root") {
                PathBuf::from(root)
                    .join(pattern)
                    .to_string_lossy()
                    .into_owned()
            } else {
                pattern
            };
            let mut matches = Vec::new();
            let entries = glob::glob(&full_pattern)
                .map_err(|e| ServiceError::InvalidInput(format!("invalid glob pattern: {e}")))?;
            for entry in entries {
                if let Ok(p) = entry {
                    matches.push(p.display().to_string());
                }
            }
            serde_json::to_value(matches).map_err(|e| ServiceError::Internal(e.to_string()))
        }
        other => Err(ServiceError::InvalidInput(format!(
            "unsupported file action '{other}'"
        ))),
    }
}
