//! Shared service session state, workflow tracking, and workflow/resource repository lookup.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use serde::{Deserialize, Serialize};
use ui_automata::WorkflowState;

use crate::ServiceError;
use crate::protocol::Target;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowTargetKey {
    pub process: Option<String>,
    pub pid: Option<u32>,
    pub title: Option<String>,
    pub selector: Option<String>,
    pub hwnd: Option<u64>,
}

impl From<&Target> for WindowTargetKey {
    fn from(t: &Target) -> Self {
        Self {
            process: t.process.clone(),
            pid: t.pid,
            title: t.title.clone(),
            selector: t.selector.clone(),
            hwnd: t.window_id.map(|id| id.as_u64()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowStatusSnapshot {
    pub running: bool,
    pub workflow_name: Option<String>,
    pub current_phase: Option<String>,
    pub started_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunLogSummary {
    pub workflow: String,
    pub log_file: String,
    pub modified_at: Option<String>,
    pub duration_ms: Option<u64>,
    pub success: Option<bool>,
}

#[derive(Default)]
pub struct WorkflowTracker {
    pub running: bool,
    pub workflow_name: Option<String>,
    pub current_phase: Option<String>,
    pub started_at: Option<String>,
    pub cancel: Option<Arc<AtomicBool>>,
    pub recent_runs: Vec<RunLogSummary>,
}

impl WorkflowTracker {
    pub fn snapshot(&self) -> WorkflowStatusSnapshot {
        WorkflowStatusSnapshot {
            running: self.running,
            workflow_name: self.workflow_name.clone(),
            current_phase: self.current_phase.clone(),
            started_at: self.started_at.clone(),
        }
    }
}

/// Resolves a workflow name or relative/absolute file path inside `workflows_dir`.
pub fn resolve_workflow_path(workflows_dir: &Path, input: &str) -> Option<PathBuf> {
    let p = PathBuf::from(input);
    if p.is_file() {
        return Some(p);
    }
    let direct = workflows_dir.join(input);
    if direct.is_file() {
        return Some(direct);
    }
    if !input.ends_with(".yml") && !input.ends_with(".yaml") {
        for ext in ["yml", "yaml"] {
            let with_ext = workflows_dir.join(format!("{input}.{ext}"));
            if with_ext.is_file() {
                return Some(with_ext);
            }
        }
    }
    let target_name = p.file_name()?.to_string_lossy().to_string();
    for rel in list_workflow_files(workflows_dir) {
        let candidate = workflows_dir.join(&rel);
        if candidate
            .file_name()
            .is_some_and(|n| n.to_string_lossy() == target_name)
        {
            return Some(candidate);
        }
    }
    None
}

pub fn list_workflow_files(workflows_dir: &Path) -> Vec<String> {
    let mut files = Vec::new();
    collect_rel_files(workflows_dir, workflows_dir, &mut files, |p| {
        matches!(
            p.extension().and_then(|e| e.to_str()),
            Some("yml" | "yaml")
        )
    });
    files.sort();
    files
}

pub fn list_resource_names(workflows_dir: &Path) -> Vec<String> {
    let mut names = Vec::new();
    collect_rel_files(workflows_dir, workflows_dir, &mut names, |p| {
        matches!(
            p.extension().and_then(|e| e.to_str()),
            Some("yml" | "yaml" | "md")
        )
    });
    names.sort();
    names
}

pub fn read_resource_content(workflows_dir: &Path, name: &str) -> Result<String, ServiceError> {
    let rel = Path::new(name.trim_start_matches("automata://"));
    if rel.is_absolute()
        || rel
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(ServiceError::InvalidInput(format!(
            "invalid resource name '{name}'"
        )));
    }
    let target = workflows_dir.join(rel);
    std::fs::read_to_string(&target).map_err(|e| {
        ServiceError::InvalidInput(format!("cannot read resource '{}': {e}", target.display()))
    })
}

pub fn collect_recent_runs(
    logs_dir: &Path,
    in_mem_runs: &[RunLogSummary],
    limit: usize,
) -> Vec<RunLogSummary> {
    let mut out = in_mem_runs.to_vec();
    if let Ok(wf_dirs) = std::fs::read_dir(logs_dir) {
        for wf_entry in wf_dirs.flatten() {
            let wf_path = wf_entry.path();
            if !wf_path.is_dir() {
                continue;
            }
            let wf_name = wf_entry.file_name().to_string_lossy().into_owned();
            if let Ok(logs) = std::fs::read_dir(&wf_path) {
                for log_entry in logs.flatten() {
                    let p = log_entry.path();
                    if p.extension().and_then(|e| e.to_str()) != Some("log") {
                        continue;
                    }
                    let path_str = p.display().to_string();
                    if out.iter().any(|r| r.log_file == path_str) {
                        continue;
                    }
                    let modified_at = p
                        .metadata()
                        .and_then(|m| m.modified())
                        .ok()
                        .map(|t| chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339());
                    out.push(RunLogSummary {
                        workflow: wf_name.clone(),
                        log_file: path_str,
                        modified_at,
                        duration_ms: None,
                        success: None,
                    });
                }
            }
        }
    }
    out.sort_by(|a, b| b.modified_at.cmp(&a.modified_at));
    out.truncate(limit);
    out
}

pub fn workflow_outputs_map(state: WorkflowState) -> HashMap<String, Vec<String>> {
    state.output.into_map()
}

fn collect_rel_files(
    root: &Path,
    dir: &Path,
    out: &mut Vec<String>,
    pred: impl Fn(&Path) -> bool + Copy,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rel_files(root, &path, out, pred);
        } else if pred(&path) {
            if let Ok(rel) = path.strip_prefix(root) {
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
}
