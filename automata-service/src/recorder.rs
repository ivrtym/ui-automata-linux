//! Session activity recorder converting executed exploration/actions into
//! validated `ui-automata` YAML workflows (`M3.4`).

use std::collections::HashMap;
use std::path::Path;

use serde_json::{Value, json};
use ui_automata::yaml::WorkflowFile;

use crate::ServiceError;
use crate::protocol::{ActCommand, InputCommand, Target};

#[derive(Debug, Default)]
pub struct RecorderState {
    pub active: bool,
    pub name: Option<String>,
    pub target: Option<Target>,
    pub started_at: Option<String>,
    pub steps: Vec<Value>,
}

impl RecorderState {
    pub fn start(&mut self, name: String, target: Option<Target>) -> Result<Value, ServiceError> {
        if self.active {
            return Err(ServiceError::Conflict(format!(
                "recorder '{}' is already active; stop it first",
                self.name.as_deref().unwrap_or("unnamed")
            )));
        }
        let started_at = chrono::Utc::now().to_rfc3339();
        self.active = true;
        self.name = Some(name.clone());
        self.target = target.clone();
        self.started_at = Some(started_at.clone());
        self.steps.clear();
        Ok(json!({
            "recording": true,
            "name": name,
            "target": target,
            "started_at": started_at,
        }))
    }

    pub fn status(&self) -> Value {
        json!({
            "recording": self.active,
            "name": self.name,
            "target": self.target,
            "started_at": self.started_at,
            "steps_count": self.steps.len(),
        })
    }

    /// Captures a successfully executed `ActCommand` into YAML workflow steps.
    pub fn record_act(&mut self, cmd: &ActCommand) -> usize {
        if !self.active {
            return self.steps.len();
        }
        match cmd {
            ActCommand::RunSteps { window, steps, .. } => {
                if self.target.is_none() {
                    self.target = Some(window.clone());
                }
                self.steps.extend(steps.iter().cloned());
            }
            ActCommand::Invoke {
                window, selector, ..
            } => {
                self.adopt_target(window);
                self.steps.push(json!({
                    "intent": format!("Invoke {selector}"),
                    "action": { "type": "Invoke", "scope": "window", "selector": selector },
                    "expect": { "type": "Always" },
                }));
            }
            ActCommand::SetValue {
                window,
                selector,
                value,
                ..
            } => {
                self.adopt_target(window);
                self.steps.push(json!({
                    "intent": format!("Set value on {selector}"),
                    "action": { "type": "SetValue", "scope": "window", "selector": selector, "value": value },
                    "expect": { "type": "Always" },
                }));
            }
            ActCommand::ClickSelector {
                window,
                selector,
                button,
                ..
            } => {
                self.adopt_target(window);
                self.steps.push(json!({
                    "intent": format!("Click {selector}"),
                    "action": { "type": "Click", "scope": "window", "selector": selector, "button": button },
                    "expect": { "type": "Always" },
                }));
            }
            ActCommand::WindowState { target, state } => {
                self.adopt_target(target);
                use crate::protocol::WindowStateArg;
                let action_type = match state {
                    WindowStateArg::Close => Some("CloseWindow"),
                    WindowStateArg::Focus => Some("ActivateWindow"),
                    WindowStateArg::Minimize => Some("MinimizeWindow"),
                    WindowStateArg::Maximize | WindowStateArg::Restore => None,
                };
                if let Some(ty) = action_type {
                    self.steps.push(json!({
                        "intent": format!("Window state {ty}"),
                        "action": { "type": ty, "scope": "window" },
                        "expect": { "type": "Always" },
                    }));
                }
            }
            ActCommand::LaunchApp { target, .. } => {
                if self.target.is_none() {
                    self.target = Some(Target::process(target.clone()));
                }
            }
            _ => {}
        }
        self.steps.len()
    }

    /// Captures pointer/keyboard events when recording low-level `InputCommand`s.
    pub fn record_input(&mut self, cmd: &InputCommand) -> usize {
        if !self.active {
            return self.steps.len();
        }
        match cmd {
            InputCommand::MouseClick { point, button } => {
                self.steps.push(json!({
                    "intent": format!("Pointer click ({}, {})", point.x, point.y),
                    "action": { "type": "Click", "x": point.x, "y": point.y, "button": button },
                    "expect": { "type": "Always" },
                }));
            }
            InputCommand::KeyPress { keys } => {
                self.steps.push(json!({
                    "intent": format!("Press key {keys}"),
                    "action": { "type": "PressKey", "key": keys },
                    "expect": { "type": "Always" },
                }));
            }
            InputCommand::TypeText { text } => {
                self.steps.push(json!({
                    "intent": "Type recorded text",
                    "action": { "type": "PointerType", "text": text },
                    "expect": { "type": "Always" },
                }));
            }
            _ => {}
        }
        self.steps.len()
    }

    fn adopt_target(&mut self, candidate: &Target) {
        if self.target.is_none() && !candidate.is_empty() {
            self.target = Some(candidate.clone());
        }
    }

    /// Stops recording, generates a complete `WorkflowFile` YAML, runs
    /// `WorkflowFile::load_from_str` + `ui_automata::lint::lint`, and optionally writes to disk.
    pub fn stop_and_export(
        &mut self,
        output_path: Option<&Path>,
        allow_writes: bool,
    ) -> Result<Value, ServiceError> {
        if !self.active {
            return Err(ServiceError::Conflict("no active recording session".into()));
        }
        let wf_name = self.name.take().unwrap_or_else(|| "recorded_workflow".into());
        let target = self.target.take();
        let mut steps = std::mem::take(&mut self.steps);
        self.active = false;
        self.started_at = None;

        if steps.is_empty() {
            steps.push(json!({
                "intent": "Placeholder recorded noop",
                "action": { "type": "NoOp" },
                "expect": { "type": "Always" },
            }));
        }

        let mut window_anchor = serde_json::Map::new();
        window_anchor.insert("type".to_string(), json!("Root"));
        let sel = target
            .as_ref()
            .and_then(|t| t.selector.as_deref())
            .or_else(|| {
                target
                    .as_ref()
                    .and_then(|t| t.title.as_deref())
                    .map(|_| "*")
            })
            .unwrap_or("*");
        if let Some(ref t) = target {
            if let Some(ref proc_name) = t.process {
                window_anchor.insert("process_name".to_string(), json!(proc_name));
            }
            if let Some(pid) = t.pid {
                window_anchor.insert("pid".to_string(), json!(pid));
            }
            if let Some(ref title) = t.title {
                if t.selector.is_none() {
                    window_anchor.insert(
                        "selector".to_string(),
                        json!(format!("[name*='{}']", title.replace('\'', "\\'"))),
                    );
                } else {
                    window_anchor.insert("selector".to_string(), json!(sel));
                }
            } else {
                window_anchor.insert("selector".to_string(), json!(sel));
            }
        } else {
            window_anchor.insert("selector".to_string(), json!("*"));
        }

        let workflow_obj = json!({
            "name": wf_name,
            "anchors": {
                "window": Value::Object(window_anchor)
            },
            "phases": [{
                "name": "recorded",
                "mount": ["window"],
                "steps": steps,
            }]
        });

        let yaml = serde_yaml::to_string(&workflow_obj)
            .map_err(|e| ServiceError::Internal(format!("YAML serialization failed: {e}")))?;

        // Validate against ui-automata YAML loader and linter before returning.
        WorkflowFile::load_from_str(&yaml, &HashMap::new())
            .map_err(|e| ServiceError::Internal(format!("exported YAML failed validation: {e}")))?;
        let diags = ui_automata::lint::lint(&yaml);

        let mut written_path: Option<String> = None;
        if let Some(out) = output_path {
            if !allow_writes {
                return Err(ServiceError::PermissionDenied(
                    "recording export to disk disabled by file write policy".into(),
                ));
            }
            if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(out, &yaml)?;
            written_path = Some(out.display().to_string());
        }

        Ok(json!({
            "recording": false,
            "name": wf_name,
            "steps_count": steps.len(),
            "valid": diags.is_empty(),
            "lint_diagnostics": diags.iter().map(|d| d.to_string()).collect::<Vec<_>>(),
            "path": written_path,
            "yaml": yaml,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_steps_and_exports_valid_linted_yaml() {
        let mut rec = RecorderState::default();
        rec.start("demo_flow".into(), Some(Target::process("gnome-calculator")))
            .unwrap();
        rec.record_act(&ActCommand::Invoke {
            window: Target::process("gnome-calculator"),
            selector: ">> [role=button][name='7']".into(),
            timeout_secs: 5.0,
        });
        let exported = rec.stop_and_export(None, true).unwrap();
        assert_eq!(exported["valid"], true);
        assert_eq!(exported["steps_count"], 1);
        let yaml = exported["yaml"].as_str().unwrap();
        assert!(yaml.contains("demo_flow"));
        assert!(yaml.contains("gnome-calculator"));
    }
}
