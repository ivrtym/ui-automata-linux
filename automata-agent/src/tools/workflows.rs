//! Thin MCP translation layer for `start_workflow`, `workflow`, and `resources`,
//! bridging [`automata_service::Event`] phase notifications to MCP `notifications/progress`.

use std::collections::HashMap;
use std::sync::Arc;

use automata_service::protocol::{Command, Event, WorkflowCommand};
use rmcp::model::ProgressNotificationParam;
use rmcp::service::{RequestContext, RoleServer};
use serde_json::Value;

use crate::state::AgentState;
use crate::tools::{opt_bool, opt_str, opt_u64, req_str};

fn json_params_to_map(args: &Value) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Some(obj) = args.get("params").and_then(|v| v.as_object()) else {
        return out;
    };
    for (k, v) in obj {
        let s = match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        out.insert(k.clone(), s);
    }
    out
}

async fn run_cmd(state: &Arc<AgentState>, cmd: Command) -> anyhow::Result<Value> {
    state
        .service
        .call_async(cmd)
        .await
        .map(|r| r.into_value())
        .map_err(|e| anyhow::anyhow!("{e}"))
}

pub async fn handle_start_workflow(
    args: Value,
    state: Arc<AgentState>,
    ctx: RequestContext<RoleServer>,
) -> anyhow::Result<Value> {
    let params = json_params_to_map(&args);
    let background = opt_bool(&args, "background", false);
    let file_arg = opt_str(&args, "file")
        .or_else(|| opt_str(&args, "local_file"))
        .or_else(|| opt_str(&args, "name"));
    let yaml_arg = opt_str(&args, "yaml");

    let wf_cmd = match (file_arg, yaml_arg) {
        (Some(file), None) => WorkflowCommand::RunFile {
            file,
            params,
            background,
        },
        (None, Some(yaml)) => WorkflowCommand::RunInline {
            yaml,
            params,
            background,
        },
        (Some(_), Some(_)) => anyhow::bail!("provide only one of 'file'/'local_file' or 'yaml'"),
        (None, None) => {
            anyhow::bail!("start_workflow requires one of 'file', 'local_file', 'name', or 'yaml'")
        }
    };

    // Bridge service PhaseStarted/Completed/Failed events to MCP notifications/progress.
    let progress_token = ctx.meta.get_progress_token();
    let progress_task = progress_token.map(|tok| {
        let mut rx = state.service.events();
        let peer = ctx.peer.clone();
        tokio::spawn(async move {
            while let Ok(evt) = rx.recv().await {
                match evt {
                    Event::PhaseStarted {
                        phase,
                        completed,
                        total,
                        ..
                    } => {
                        let _ = peer
                            .notify_progress(ProgressNotificationParam {
                                progress_token: tok.clone(),
                                progress: (completed as f64).min(total as f64),
                                total: Some(total as f64),
                                message: Some(format!("Phase started: {phase}")),
                            })
                            .await;
                    }
                    Event::PhaseCompleted {
                        phase,
                        completed,
                        total,
                        skipped,
                        ..
                    } => {
                        let label = if skipped { "skipped" } else { "completed" };
                        let _ = peer
                            .notify_progress(ProgressNotificationParam {
                                progress_token: tok.clone(),
                                progress: (completed as f64).min(total as f64),
                                total: Some(total as f64),
                                message: Some(format!("Phase {label}: {phase}")),
                            })
                            .await;
                    }
                    Event::PhaseFailed { phase, error, .. } => {
                        let _ = peer
                            .notify_progress(ProgressNotificationParam {
                                progress_token: tok.clone(),
                                progress: 0.0,
                                total: None,
                                message: Some(format!("Phase failed ({phase}): {error}")),
                            })
                            .await;
                    }
                    Event::WorkflowFinished { .. } => break,
                    _ => {}
                }
            }
        })
    });

    let result = run_cmd(&state, Command::Workflow(wf_cmd)).await;
    if let Some(handle) = progress_task {
        handle.abort();
    }
    result
}

pub async fn handle_workflow(args: Value, state: Arc<AgentState>) -> anyhow::Result<Value> {
    let action = req_str(&args, "action")?;
    let cmd = match action.as_str() {
        "list" => Command::Workflow(WorkflowCommand::List),
        "read" => {
            let name = opt_str(&args, "name")
                .or_else(|| opt_str(&args, "file"))
                .ok_or_else(|| anyhow::anyhow!("workflow read requires 'name' or 'file'"))?;
            Command::Workflow(WorkflowCommand::Read { name })
        }
        "lint" => Command::Workflow(WorkflowCommand::Lint {
            yaml: opt_str(&args, "yaml"),
            file: opt_str(&args, "file").or_else(|| opt_str(&args, "name")),
        }),
        "status" => Command::Workflow(WorkflowCommand::Status),
        "cancel" => Command::Workflow(WorkflowCommand::Cancel),
        "list_runs" | "logs" => Command::Workflow(WorkflowCommand::ListRuns {
            limit: opt_u64(&args, "limit").unwrap_or(20) as usize,
        }),
        other => anyhow::bail!("unsupported workflow action '{other}'"),
    };
    run_cmd(&state, cmd).await
}

pub async fn handle_resources(args: Value, state: Arc<AgentState>) -> anyhow::Result<Value> {
    let action = req_str(&args, "action")?;
    let cmd = match action.as_str() {
        "list" => Command::Workflow(WorkflowCommand::ListResources),
        "read" => Command::Workflow(WorkflowCommand::ReadResource {
            name: req_str(&args, "name")?,
        }),
        other => anyhow::bail!("unsupported resources action '{other}'"),
    };
    run_cmd(&state, cmd).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use automata_linux::ConnectionMode;
    use serde_json::json;

    #[tokio::test]
    async fn workflow_and_resources_handlers_work_in_temp_dir() {
        let tmp = std::env::temp_dir().join(format!("automata_agent_wf_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("linux")).unwrap();
        std::fs::write(tmp.join("linux/AGENT.md"), "# Linux Agent\n").unwrap();
        let sample_yml = r#"
name: sample_wf
anchors:
  win: { type: Root, selector: "*" }
phases:
  - name: p1
    mount: [win]
    steps:
      - intent: noop
        action: { type: NoOp }
        expect: { type: Always }
"#;
        std::fs::write(tmp.join("linux/sample.yml"), sample_yml).unwrap();

        let state = AgentState::new(ConnectionMode::OneShot, Some(tmp.clone()));

        let listed = handle_workflow(json!({ "action": "list" }), state.clone())
            .await
            .unwrap();
        assert_eq!(listed["workflows"], json!(["linux/sample.yml"]));

        let lint_ok = handle_workflow(
            json!({ "action": "lint", "yaml": sample_yml }),
            state.clone(),
        )
        .await
        .unwrap();
        assert_eq!(lint_ok["valid"], json!(true));

        let lint_bad = handle_workflow(
            json!({
                "action": "lint",
                "yaml": "name: bad\nphases:\n  - name: p\n    steps:\n      - intent: x\n        action: { type: BogusAction }\n        expect: { type: Always }\n"
            }),
            state.clone(),
        )
        .await
        .unwrap();
        assert_eq!(lint_bad["valid"], json!(false));
        assert!(lint_bad["error_count"].as_u64().unwrap() > 0);

        let res_list = handle_resources(json!({ "action": "list" }), state.clone())
            .await
            .unwrap();
        assert_eq!(
            res_list,
            json!(["linux/AGENT.md", "linux/sample.yml"])
        );

        let md = handle_resources(
            json!({ "action": "read", "name": "linux/AGENT.md" }),
            state.clone(),
        )
        .await
        .unwrap();
        assert_eq!(md["content"], json!("# Linux Agent\n"));

        let status = handle_workflow(json!({ "action": "status" }), state.clone())
            .await
            .unwrap();
        assert_eq!(status["running"], json!(false));

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
