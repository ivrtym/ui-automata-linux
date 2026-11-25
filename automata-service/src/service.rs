//! Core `AutomationService` implementing the three-lane scheduler
//! (UI Actor thread, parallel IO pool, cancellable Workflow Runner) and unified
//! `Command` / `Event` protocol.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use automata_linux::coords::CoordSpace;
use automata_linux::diagnostics::CapabilitiesSnapshot;
use automata_linux::{ElementNode, query_element_snapshot};
use serde_json::{Value, json};
use tokio::sync::broadcast;
use ui_automata::yaml::WorkflowFile;
use ui_automata::{Executor, PhaseEvent};

use crate::ServiceError;
use crate::actor::{LinuxUiActorOps, SingleStepKind, UiActorHandle, UiActorOps, single_action_step};
use crate::file_ops::execute_file_request;
use crate::protocol::{
    ActCommand, Command, CommandResult, ConnectionLifecycle, Event, ExploreCommand, FileCommand,
    InputCommand, ServiceConfig, ServiceInfo, SessionCommand, WorkflowCommand,
};
use crate::recorder::RecorderState;
use crate::session::{
    RunLogSummary, WorkflowStatusSnapshot, WorkflowTracker, collect_recent_runs,
    list_resource_names, list_workflow_files, read_resource_content, resolve_workflow_path,
    workflow_outputs_map,
};
use crate::system_ops::{
    execute_clipboard_command, execute_input_command, execute_system_command,
};

const EVENT_CHANNEL_CAPACITY: usize = 256;

struct Inner {
    config: ServiceConfig,
    ui_actor: UiActorHandle,
    tracker: Mutex<WorkflowTracker>,
    recorder: Mutex<RecorderState>,
    events_tx: broadcast::Sender<Event>,
    next_command_id: AtomicU64,
}

/// Unified, session-aware automation service shared by Rust callers, CLI/daemon,
/// and `automata-agent` MCP adapters.
#[derive(Clone)]
pub struct AutomationService {
    inner: Arc<Inner>,
}

/// Handle to a submitted background command/workflow returned by [`AutomationService::submit`].
pub struct CommandHandle {
    pub command_id: u64,
    rx: tokio::sync::oneshot::Receiver<Result<CommandResult, ServiceError>>,
    cancel_flag: Option<Arc<AtomicBool>>,
}

impl CommandHandle {
    pub async fn wait_async(self) -> Result<CommandResult, ServiceError> {
        self.rx
            .await
            .map_err(|_| ServiceError::Internal("submitted command dropped completion".into()))?
    }

    pub fn wait(self) -> Result<CommandResult, ServiceError> {
        self.rx
            .blocking_recv()
            .map_err(|_| ServiceError::Internal("submitted command dropped completion".into()))?
    }

    pub fn cancel(&self) -> bool {
        if let Some(flag) = &self.cancel_flag {
            flag.store(true, Ordering::Relaxed);
            return true;
        }
        false
    }
}

impl AutomationService {
    /// Opens a live Linux [`AutomationService`] backed by persistent/oneshot AT-SPI2 + X11/Wayland actors.
    pub fn open(config: ServiceConfig) -> Result<Self, ServiceError> {
        let mode = config.connection_mode.into();
        Ok(Self::open_with_actor(config, move || {
            Box::new(LinuxUiActorOps::new(mode))
        }))
    }

    /// Opens an [`AutomationService`] with a custom [`UiActorOps`] backend (used for fake/contract testing).
    pub fn open_with_actor<F>(config: ServiceConfig, actor_factory: F) -> Self
    where
        F: FnOnce() -> Box<dyn UiActorOps> + Send + 'static,
    {
        let (events_tx, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        Self {
            inner: Arc::new(Inner {
                config,
                ui_actor: UiActorHandle::spawn_with_factory(actor_factory),
                tracker: Mutex::new(WorkflowTracker::default()),
                recorder: Mutex::new(RecorderState::default()),
                events_tx,
                next_command_id: AtomicU64::new(1),
            }),
        }
    }

    pub fn config(&self) -> &ServiceConfig {
        &self.inner.config
    }

    /// Subscribes to structured command, workflow phase, and recorder events.
    pub fn events(&self) -> broadcast::Receiver<Event> {
        self.inner.events_tx.subscribe()
    }

    /// Returns a combined service status and platform capabilities snapshot.
    pub fn info(&self) -> ServiceInfo {
        let capabilities = self
            .inner
            .ui_actor
            .call_sync(|ops| Ok(ops.capabilities()))
            .unwrap_or_else(|e| fallback_caps(e.to_string()));
        let recording = self
            .inner
            .recorder
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .active;
        let workflow_running = self
            .inner
            .tracker
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .running;

        ServiceInfo {
            version: env!("CARGO_PKG_VERSION").to_string(),
            connection_mode: self.inner.config.connection_mode,
            workflows_dir: self.inner.config.workflows_dir.display().to_string(),
            logs_dir: self.inner.config.logs_dir.display().to_string(),
            allow_file_writes: self.inner.config.allow_file_writes,
            recording,
            workflow_running,
            capabilities,
        }
    }

    /// Synchronous blocking execution entrypoint for Rust libraries, CLI, and tests.
    ///
    /// Safe to call both outside and inside an active Tokio runtime without nested-runtime panics.
    pub fn call(&self, cmd: Command) -> Result<CommandResult, ServiceError> {
        let cmd_id = self.inner.next_command_id.fetch_add(1, Ordering::Relaxed);
        let kind = cmd.kind_label().to_string();
        let _ = self.inner.events_tx.send(Event::CommandStarted {
            command_id: cmd_id,
            kind: kind.clone(),
        });

        let res = self.execute_sync(cmd);
        let _ = self.inner.events_tx.send(Event::CommandFinished {
            command_id: cmd_id,
            kind,
            ok: res.is_ok(),
        });
        res
    }

    /// Async non-blocking execution entrypoint for async MCP/HTTP servers and daemons.
    pub async fn call_async(&self, cmd: Command) -> Result<CommandResult, ServiceError> {
        let cmd_id = self.inner.next_command_id.fetch_add(1, Ordering::Relaxed);
        let kind = cmd.kind_label().to_string();
        let _ = self.inner.events_tx.send(Event::CommandStarted {
            command_id: cmd_id,
            kind: kind.clone(),
        });

        let res = self.execute_async(cmd).await;
        let _ = self.inner.events_tx.send(Event::CommandFinished {
            command_id: cmd_id,
            kind,
            ok: res.is_ok(),
        });
        res
    }

    /// Submits a command for background execution and returns a [`CommandHandle`].
    pub fn submit(&self, cmd: Command) -> CommandHandle {
        let cmd_id = self.inner.next_command_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = tokio::sync::oneshot::channel();
        let svc = self.clone();
        let cancel_flag = matches!(
            cmd,
            Command::Workflow(WorkflowCommand::RunFile { .. } | WorkflowCommand::RunInline { .. })
        )
        .then(|| Arc::new(AtomicBool::new(false)));

        let flag_for_thread = cancel_flag.clone();
        std::thread::Builder::new()
            .name(format!("automata-cmd-{cmd_id}"))
            .spawn(move || {
                let res = match cmd {
                    Command::Workflow(WorkflowCommand::RunFile {
                        file,
                        params,
                        background,
                    }) => svc.run_workflow_internal(Some(file), None, params, background, flag_for_thread),
                    Command::Workflow(WorkflowCommand::RunInline {
                        yaml,
                        params,
                        background,
                    }) => svc.run_workflow_internal(None, Some(yaml), params, background, flag_for_thread),
                    other => svc.call(other),
                };
                let _ = tx.send(res);
            })
            .expect("spawn automata submitted command thread");

        CommandHandle {
            command_id: cmd_id,
            rx,
            cancel_flag,
        }
    }

    // ── Helper methods directly usable by Service/MCP resource handlers ───────

    pub fn workflow_status(&self) -> WorkflowStatusSnapshot {
        self.inner
            .tracker
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .snapshot()
    }

    pub fn cancel_active_workflow(&self) -> bool {
        let guard = self.inner.tracker.lock().unwrap_or_else(|e| e.into_inner());
        if guard.running {
            if let Some(flag) = &guard.cancel {
                flag.store(true, Ordering::Relaxed);
                return true;
            }
        }
        false
    }

    pub fn list_workflow_files(&self) -> Vec<String> {
        list_workflow_files(&self.inner.config.workflows_dir)
    }

    pub fn list_resource_names(&self) -> Vec<String> {
        list_resource_names(&self.inner.config.workflows_dir)
    }

    pub fn read_resource_content(&self, name: &str) -> Result<String, ServiceError> {
        read_resource_content(&self.inner.config.workflows_dir, name)
    }

    pub fn collect_recent_runs(&self, limit: usize) -> Vec<RunLogSummary> {
        let in_mem = self
            .inner
            .tracker
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .recent_runs
            .clone();
        collect_recent_runs(&self.inner.config.logs_dir, &in_mem, limit)
    }

    // ── Three-lane execution routing ──────────────────────────────────────────

    fn execute_sync(&self, cmd: Command) -> Result<CommandResult, ServiceError> {
        match cmd {
            Command::Session(sc) => self.handle_session(sc),
            Command::Explore(ec) => self.handle_explore_sync(ec),
            Command::Act(ac) => self.handle_act_sync(ac),
            Command::Vision(vc) => {
                let v = self.inner.ui_actor.call_sync(move |ops| ops.vision(&vc))?;
                Ok(CommandResult::from_value(v))
            }
            Command::Browser(bc) => {
                let v = self.inner.ui_actor.call_sync(move |ops| ops.browser(&bc))?;
                Ok(CommandResult::from_value(v))
            }
            Command::Input(ic) => self.handle_input_sync(ic),
            Command::Clipboard(cc) => Ok(CommandResult::from_value(execute_clipboard_command(&cc)?)),
            Command::File(FileCommand::Raw(raw)) => Ok(CommandResult::from_value(
                execute_file_request(raw, self.inner.config.allow_file_writes)?,
            )),
            Command::System(sc) => Ok(CommandResult::from_value(execute_system_command(&sc)?)),
            Command::Workflow(wc) => self.handle_workflow_sync(wc),
        }
    }

    async fn execute_async(&self, cmd: Command) -> Result<CommandResult, ServiceError> {
        match cmd {
            Command::Session(sc) => {
                let svc = self.clone();
                run_blocking_pool(move || svc.handle_session(sc)).await
            }
            Command::Explore(ec) => self.handle_explore_async(ec).await,
            Command::Act(ac) => self.handle_act_async(ac).await,
            Command::Vision(vc) => {
                let v = self.inner.ui_actor.call_async(move |ops| ops.vision(&vc)).await?;
                Ok(CommandResult::from_value(v))
            }
            Command::Browser(bc) => {
                let v = self.inner.ui_actor.call_async(move |ops| ops.browser(&bc)).await?;
                Ok(CommandResult::from_value(v))
            }
            Command::Input(ic) => {
                let svc = self.clone();
                run_blocking_pool(move || svc.handle_input_sync(ic)).await
            }
            Command::Clipboard(cc) => {
                run_blocking_pool(move || {
                    Ok(CommandResult::from_value(execute_clipboard_command(&cc)?))
                })
                .await
            }
            Command::File(FileCommand::Raw(raw)) => {
                let allow = self.inner.config.allow_file_writes;
                run_blocking_pool(move || {
                    Ok(CommandResult::from_value(execute_file_request(raw, allow)?))
                })
                .await
            }
            Command::System(sc) => {
                run_blocking_pool(move || Ok(CommandResult::from_value(execute_system_command(&sc)?)))
                    .await
            }
            Command::Workflow(wc) => self.handle_workflow_async(wc).await,
        }
    }

    fn handle_session(&self, cmd: SessionCommand) -> Result<CommandResult, ServiceError> {
        match cmd {
            SessionCommand::Info => CommandResult::serialize_result(&self.info()),
            SessionCommand::Capabilities => {
                let caps = self.inner.ui_actor.call_sync(|ops| Ok(ops.capabilities()))?;
                CommandResult::serialize_result(&caps)
            }
            SessionCommand::ResetShadowDom => {
                self.inner.ui_actor.call_sync(|ops| {
                    ops.reset_shadow_dom();
                    Ok(())
                })?;
                Ok(CommandResult::from_value(json!({ "reset": true })))
            }
            SessionCommand::RecordStart { name, target } => {
                let out = self
                    .inner
                    .recorder
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .start(name, target)?;
                let _ = self.inner.events_tx.send(Event::RecorderUpdated {
                    recording: true,
                    steps_count: 0,
                });
                Ok(CommandResult::from_value(out))
            }
            SessionCommand::RecordStatus => {
                let out = self
                    .inner
                    .recorder
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .status();
                Ok(CommandResult::from_value(out))
            }
            SessionCommand::RecordStop { output_path } => {
                let allow_writes = self.inner.config.allow_file_writes;
                let out = self
                    .inner
                    .recorder
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .stop_and_export(output_path.as_deref(), allow_writes)?;
                let steps_count = out["steps_count"].as_u64().unwrap_or(0) as usize;
                let _ = self.inner.events_tx.send(Event::RecorderUpdated {
                    recording: false,
                    steps_count,
                });
                Ok(CommandResult::from_value(out))
            }
        }
    }

    fn handle_explore_sync(&self, cmd: ExploreCommand) -> Result<CommandResult, ServiceError> {
        let prepared = self.prepare_explore_command(cmd)?;
        if let ExploreCommand::QuerySnapshot {
            bundle_dir,
            selector,
            max_results,
        } = prepared
        {
            return Ok(CommandResult::from_value(execute_query_snapshot(
                &bundle_dir,
                &selector,
                max_results,
            )?));
        }
        let v = self
            .inner
            .ui_actor
            .call_sync(move |ops| dispatch_explore(ops, prepared))?;
        Ok(CommandResult::from_value(v))
    }

    async fn handle_explore_async(&self, cmd: ExploreCommand) -> Result<CommandResult, ServiceError> {
        let prepared = self.prepare_explore_command(cmd)?;
        if let ExploreCommand::QuerySnapshot {
            bundle_dir,
            selector,
            max_results,
        } = prepared
        {
            return run_blocking_pool(move || {
                Ok(CommandResult::from_value(execute_query_snapshot(
                    &bundle_dir,
                    &selector,
                    max_results,
                )?))
            })
            .await;
        }
        let v = self
            .inner
            .ui_actor
            .call_async(move |ops| dispatch_explore(ops, prepared))
            .await?;
        Ok(CommandResult::from_value(v))
    }

    fn prepare_explore_command(&self, cmd: ExploreCommand) -> Result<ExploreCommand, ServiceError> {
        match cmd {
            ExploreCommand::CaptureBundle {
                target,
                out_dir,
                max_nodes,
                max_depth,
                include_ocr,
            } => {
                let resolved = resolve_snapshot_dir(
                    out_dir,
                    &self.inner.config.logs_dir,
                    self.inner.config.allow_file_writes,
                    "bundle",
                )?;
                Ok(ExploreCommand::CaptureBundle {
                    target,
                    out_dir: Some(resolved),
                    max_nodes,
                    max_depth,
                    include_ocr,
                })
            }
            ExploreCommand::Batch {
                target,
                steps,
                stop_on_error,
                bundle_dir,
            } => {
                let needs_bundle = bundle_dir.is_some()
                    || steps
                        .iter()
                        .any(|s| s.capture_tree || s.capture_screenshot || s.capture_ocr);
                let resolved = if needs_bundle {
                    Some(resolve_snapshot_dir(
                        bundle_dir,
                        &self.inner.config.logs_dir,
                        self.inner.config.allow_file_writes,
                        "batch",
                    )?)
                } else {
                    None
                };
                Ok(ExploreCommand::Batch {
                    target,
                    steps,
                    stop_on_error,
                    bundle_dir: resolved,
                })
            }
            other => Ok(other),
        }
    }

    fn handle_act_sync(&self, cmd: ActCommand) -> Result<CommandResult, ServiceError> {
        if let ActCommand::ListInstalledApps { filter } = &cmd {
            let v = execute_list_installed_apps(filter.as_deref())?;
            return Ok(CommandResult::from_value(v));
        }
        if let ActCommand::KillApp { pid, process } = &cmd {
            let v = execute_kill_app(*pid, process.as_deref())?;
            return Ok(CommandResult::from_value(v));
        }

        let cmd_for_record = cmd.clone();
        let v = self.inner.ui_actor.call_sync(move |ops| dispatch_act(ops, cmd))?;
        self.record_act_if_active(&cmd_for_record);
        Ok(CommandResult::from_value(v))
    }

    async fn handle_act_async(&self, cmd: ActCommand) -> Result<CommandResult, ServiceError> {
        if let ActCommand::ListInstalledApps { filter } = cmd {
            return run_blocking_pool(move || {
                Ok(CommandResult::from_value(execute_list_installed_apps(
                    filter.as_deref(),
                )?))
            })
            .await;
        }
        if let ActCommand::KillApp { pid, process } = cmd {
            return run_blocking_pool(move || {
                Ok(CommandResult::from_value(execute_kill_app(
                    pid,
                    process.as_deref(),
                )?))
            })
            .await;
        }

        let cmd_for_record = cmd.clone();
        let v = self
            .inner
            .ui_actor
            .call_async(move |ops| dispatch_act(ops, cmd))
            .await?;
        self.record_act_if_active(&cmd_for_record);
        Ok(CommandResult::from_value(v))
    }

    fn record_act_if_active(&self, cmd: &ActCommand) {
        let mut rec = self.inner.recorder.lock().unwrap_or_else(|e| e.into_inner());
        if rec.active {
            let count = rec.record_act(cmd);
            let _ = self.inner.events_tx.send(Event::RecorderUpdated {
                recording: true,
                steps_count: count,
            });
        }
    }

    fn handle_input_sync(&self, cmd: InputCommand) -> Result<CommandResult, ServiceError> {
        let actor = &self.inner.ui_actor;
        let v = execute_input_command(&cmd, |wid| actor.call_sync(move |ops| ops.resolve_window_bounds(wid)))?;
        let mut rec = self.inner.recorder.lock().unwrap_or_else(|e| e.into_inner());
        if rec.active {
            let count = rec.record_input(&cmd);
            let _ = self.inner.events_tx.send(Event::RecorderUpdated {
                recording: true,
                steps_count: count,
            });
        }
        Ok(CommandResult::from_value(v))
    }

    fn handle_workflow_sync(&self, cmd: WorkflowCommand) -> Result<CommandResult, ServiceError> {
        match cmd {
            WorkflowCommand::RunFile {
                file,
                params,
                background,
            } => self.run_workflow_internal(Some(file), None, params, background, None),
            WorkflowCommand::RunInline {
                yaml,
                params,
                background,
            } => self.run_workflow_internal(None, Some(yaml), params, background, None),
            other => self.handle_workflow_query(other),
        }
    }

    async fn handle_workflow_async(&self, cmd: WorkflowCommand) -> Result<CommandResult, ServiceError> {
        match cmd {
            WorkflowCommand::RunFile {
                file,
                params,
                background,
            } => {
                let svc = self.clone();
                run_blocking_pool(move || {
                    svc.run_workflow_internal(Some(file), None, params, background, None)
                })
                .await
            }
            WorkflowCommand::RunInline {
                yaml,
                params,
                background,
            } => {
                let svc = self.clone();
                run_blocking_pool(move || {
                    svc.run_workflow_internal(None, Some(yaml), params, background, None)
                })
                .await
            }
            other => {
                let svc = self.clone();
                run_blocking_pool(move || svc.handle_workflow_query(other)).await
            }
        }
    }

    fn handle_workflow_query(&self, cmd: WorkflowCommand) -> Result<CommandResult, ServiceError> {
        match cmd {
            WorkflowCommand::List => Ok(CommandResult::from_value(json!({
                "workflows_dir": self.inner.config.workflows_dir.display().to_string(),
                "workflows": self.list_workflow_files(),
            }))),
            WorkflowCommand::Read { name } => {
                let path = resolve_workflow_path(&self.inner.config.workflows_dir, &name).ok_or_else(
                    || ServiceError::InvalidInput(format!("workflow '{name}' not found")),
                )?;
                let content = std::fs::read_to_string(&path)?;
                Ok(CommandResult::from_value(json!({
                    "path": path.display().to_string(),
                    "content": content,
                })))
            }
            WorkflowCommand::Lint { yaml, file } => {
                let source = match (yaml, file) {
                    (Some(y), _) => y,
                    (None, Some(f)) => {
                        let p = resolve_workflow_path(&self.inner.config.workflows_dir, &f)
                            .ok_or_else(|| {
                                ServiceError::InvalidInput(format!("workflow '{f}' not found"))
                            })?;
                        std::fs::read_to_string(p)?
                    }
                    (None, None) => {
                        return Err(ServiceError::InvalidInput(
                            "workflow lint requires 'yaml' or 'file'".into(),
                        ));
                    }
                };
                let diags = ui_automata::lint::lint(&source);
                let formatted: Vec<Value> = diags
                    .iter()
                    .map(|d| {
                        json!({
                            "line": d.line,
                            "col": d.col,
                            "path": d.path,
                            "message": d.message,
                            "display": d.to_string(),
                        })
                    })
                    .collect();
                Ok(CommandResult::from_value(json!({
                    "valid": diags.is_empty(),
                    "error_count": diags.len(),
                    "diagnostics": formatted,
                })))
            }
            WorkflowCommand::Status => CommandResult::serialize_result(&self.workflow_status()),
            WorkflowCommand::Cancel => Ok(CommandResult::from_value(json!({
                "cancelled": self.cancel_active_workflow(),
            }))),
            WorkflowCommand::ListRuns { limit } => {
                CommandResult::serialize_result(&self.collect_recent_runs(limit))
            }
            WorkflowCommand::ListResources => {
                CommandResult::serialize_result(&self.list_resource_names())
            }
            WorkflowCommand::ReadResource { name } => {
                let content = self.read_resource_content(&name)?;
                Ok(CommandResult::from_value(json!({
                    "name": name,
                    "content": content,
                })))
            }
            WorkflowCommand::RunFile { .. } | WorkflowCommand::RunInline { .. } => {
                unreachable!("handled by run_workflow_internal")
            }
        }
    }

    fn run_workflow_internal(
        &self,
        file: Option<String>,
        yaml: Option<String>,
        params: HashMap<String, String>,
        background: bool,
        external_cancel: Option<Arc<AtomicBool>>,
    ) -> Result<CommandResult, ServiceError> {
        let (wf, source_desc) = match (file, yaml) {
            (Some(f), None) => {
                let resolved = resolve_workflow_path(&self.inner.config.workflows_dir, &f)
                    .ok_or_else(|| ServiceError::InvalidInput(format!("workflow '{f}' not found")))?;
                let path_str = resolved.to_string_lossy().into_owned();
                let wf = WorkflowFile::load(&path_str, &params)
                    .map_err(|e| ServiceError::InvalidInput(e.to_string()))?;
                (wf, path_str)
            }
            (None, Some(y)) => {
                let wf = WorkflowFile::load_from_str(&y, &params)
                    .map_err(|e| ServiceError::InvalidInput(e.to_string()))?;
                (wf, "<inline yaml>".into())
            }
            _ => {
                return Err(ServiceError::InvalidInput(
                    "workflow run requires exactly one of 'file' or 'yaml'".into(),
                ));
            }
        };

        let wf_name = wf.name.clone();
        let started_at = chrono::Utc::now().to_rfc3339();
        let cancel_flag = external_cancel.unwrap_or_else(|| Arc::new(AtomicBool::new(false)));

        let log_dir = self.inner.config.logs_dir.join(&wf_name);
        let _ = std::fs::create_dir_all(&log_dir);
        let ts = chrono::Utc::now().format("%Y%m%dT%H%M%S");
        let log_path = log_dir.join(format!("{ts}.log"));
        let log_file_str = log_path.display().to_string();

        {
            let mut tracker = self.inner.tracker.lock().unwrap_or_else(|e| e.into_inner());
            if tracker.running {
                return Err(ServiceError::Conflict(format!(
                    "another workflow '{}' is already running; call workflow(cancel) first",
                    tracker.workflow_name.as_deref().unwrap_or("unknown")
                )));
            }
            tracker.running = true;
            tracker.workflow_name = Some(wf_name.clone());
            tracker.current_phase = Some("initializing".into());
            tracker.started_at = Some(started_at.clone());
            tracker.cancel = Some(cancel_flag.clone());
        }

        let total_phases = wf.phases.len().max(1);
        let svc = self.clone();
        let wf_name_bg = wf_name.clone();
        let log_file_bg = log_file_str.clone();
        let conn_mode = self.inner.config.connection_mode.into();
        let (done_tx, done_rx) = std::sync::mpsc::channel();

        std::thread::Builder::new()
            .name(format!("workflow-{wf_name}"))
            .spawn(move || {
                let started = Instant::now();
                let desktop = automata_linux::Desktop::with_connection_mode(conn_mode);
                let _cap_scope = Some(desktop.x11_capture_session());
                let mut executor = Executor::new(desktop);
                let mut completed_count = 0usize;
                let mut phase_log = Vec::<String>::new();

                let mut on_event = |evt: PhaseEvent| {
                    match &evt {
                        PhaseEvent::PhaseStarted(p) => {
                            phase_log.push(format!("Phase started: {p}"));
                            if let Ok(mut guard) = svc.inner.tracker.lock() {
                                guard.current_phase = Some(p.clone());
                            }
                            let _ = svc.inner.events_tx.send(Event::PhaseStarted {
                                workflow: wf_name_bg.clone(),
                                phase: p.clone(),
                                completed: completed_count,
                                total: total_phases,
                            });
                        }
                        PhaseEvent::PhaseCompleted(p) => {
                            completed_count += 1;
                            phase_log.push(format!("Phase completed: {p}"));
                            let _ = svc.inner.events_tx.send(Event::PhaseCompleted {
                                workflow: wf_name_bg.clone(),
                                phase: p.clone(),
                                completed: completed_count,
                                total: total_phases,
                                skipped: false,
                            });
                        }
                        PhaseEvent::PhaseSkipped(p) => {
                            completed_count += 1;
                            phase_log.push(format!("Phase skipped: {p}"));
                            let _ = svc.inner.events_tx.send(Event::PhaseCompleted {
                                workflow: wf_name_bg.clone(),
                                phase: p.clone(),
                                completed: completed_count,
                                total: total_phases,
                                skipped: true,
                            });
                        }
                        PhaseEvent::PhaseFailed { phase, error } => {
                            phase_log.push(format!("Phase failed ({phase}): {error}"));
                            let _ = svc.inner.events_tx.send(Event::PhaseFailed {
                                workflow: wf_name_bg.clone(),
                                phase: phase.clone(),
                                error: error.clone(),
                            });
                        }
                        PhaseEvent::Completed => phase_log.push("Workflow completed".into()),
                        PhaseEvent::Failed(e) => phase_log.push(format!("Workflow failed: {e}")),
                    }
                };

                let run_res = wf.run(&mut executor, Some(&mut on_event), Some(&cancel_flag));
                executor.desktop.close_persistent_connections();

                let duration_ms = started.elapsed().as_millis() as u64;
                let success = run_res.is_ok();
                let _ = std::fs::write(&log_path, phase_log.join("\n"));

                {
                    let mut tracker = svc.inner.tracker.lock().unwrap_or_else(|e| e.into_inner());
                    tracker.running = false;
                    tracker.current_phase = None;
                    tracker.cancel = None;
                    tracker.recent_runs.insert(
                        0,
                        RunLogSummary {
                            workflow: wf_name_bg.clone(),
                            log_file: log_file_bg.clone(),
                            modified_at: Some(chrono::Utc::now().to_rfc3339()),
                            duration_ms: Some(duration_ms),
                            success: Some(success),
                        },
                    );
                    if tracker.recent_runs.len() > 50 {
                        tracker.recent_runs.truncate(50);
                    }
                }

                let _ = svc.inner.events_tx.send(Event::WorkflowFinished {
                    workflow: wf_name_bg.clone(),
                    ok: success,
                    duration_ms,
                });

                let out = match run_res {
                    Ok(wf_state) => Ok(CommandResult::from_value(json!({
                        "ok": true,
                        "workflow": wf_name_bg,
                        "source": source_desc,
                        "duration_ms": duration_ms,
                        "log_file": log_file_bg,
                        "outputs": workflow_outputs_map(wf_state),
                    }))),
                    Err(e) => Err(ServiceError::classify_message(e.to_string())),
                };
                let _ = done_tx.send(out);
            })
            .map_err(|e| ServiceError::Internal(format!("spawn workflow runner failed: {e}")))?;

        if background {
            return Ok(CommandResult::from_value(json!({
                "started": true,
                "workflow": wf_name,
                "started_at": started_at,
                "log_file": log_file_str,
            })));
        }

        done_rx
            .recv()
            .map_err(|_| ServiceError::Internal("workflow runner dropped completion".into()))?
    }
}

fn dispatch_explore(ops: &mut dyn UiActorOps, cmd: ExploreCommand) -> Result<Value, ServiceError> {
    match cmd {
        ExploreCommand::ListWindows { filter } => ops.list_windows(&filter),
        ExploreCommand::FocusedWindow => ops.focused_window(),
        ExploreCommand::WindowTree { target, max_results } => ops.window_tree(&target, max_results),
        ExploreCommand::ActiveWindowTree {
            selector,
            max_results,
        } => ops.active_window_tree(selector.as_deref(), max_results),
        ExploreCommand::FindElements {
            target,
            selector,
            include_ancestors,
            include_siblings,
            max_results,
        } => ops.find_elements(
            &target,
            &selector,
            include_ancestors,
            include_siblings,
            max_results,
        ),
        ExploreCommand::CaptureBundle {
            target,
            out_dir,
            max_nodes,
            max_depth,
            include_ocr,
        } => {
            let dir = out_dir.ok_or_else(|| ServiceError::Internal("missing bundle dir".into()))?;
            ops.capture_bundle(&target, &dir, max_nodes, max_depth, include_ocr)
        }
        ExploreCommand::QuerySnapshot {
            bundle_dir,
            selector,
            max_results,
        } => execute_query_snapshot(&bundle_dir, &selector, max_results),
        ExploreCommand::Batch {
            target,
            steps,
            stop_on_error,
            bundle_dir,
        } => ops.batch_explore(&target, &steps, stop_on_error, bundle_dir.as_deref()),
    }
}

fn resolve_snapshot_dir(
    explicit: Option<PathBuf>,
    logs_dir: &Path,
    allow_writes: bool,
    prefix: &str,
) -> Result<PathBuf, ServiceError> {
    if let Some(dir) = explicit {
        if !allow_writes {
            return Err(ServiceError::PermissionDenied(
                "custom snapshot output path is disabled on this MCP server (pass --allow-remote-file-writes)".into(),
            ));
        }
        return Ok(dir);
    }
    let ts = chrono::Utc::now().format("%Y%m%dT%H%M%S_%3f");
    Ok(logs_dir.join("snapshots").join(format!("{prefix}_{ts}")))
}

fn execute_query_snapshot(
    bundle_dir: &Path,
    selector: &str,
    max_results: usize,
) -> Result<Value, ServiceError> {
    let tree_path = if bundle_dir.is_file() {
        bundle_dir.to_path_buf()
    } else {
        let direct = bundle_dir.join("tree.json");
        if direct.is_file() {
            direct
        } else {
            let manifest_path = bundle_dir.join("manifest.json");
            let manifest_raw = std::fs::read_to_string(&manifest_path).map_err(|e| {
                ServiceError::InvalidInput(format!(
                    "snapshot bundle '{}' has no tree.json or readable manifest.json: {e}",
                    bundle_dir.display()
                ))
            })?;
            let manifest: Value = serde_json::from_str(&manifest_raw)
                .map_err(|e| ServiceError::InvalidInput(format!("invalid manifest.json: {e}")))?;
            let p = manifest["tree_path"].as_str().ok_or_else(|| {
                ServiceError::InvalidInput("snapshot bundle manifest has no 'tree_path'".into())
            })?;
            PathBuf::from(p)
        }
    };

    let raw = std::fs::read_to_string(&tree_path).map_err(|e| {
        ServiceError::InvalidInput(format!(
            "failed to read snapshot tree '{}': {e}",
            tree_path.display()
        ))
    })?;
    let root: ElementNode = serde_json::from_str(&raw).map_err(|e| {
        ServiceError::InvalidInput(format!(
            "invalid ElementNode snapshot '{}': {e}",
            tree_path.display()
        ))
    })?;
    let hits = query_element_snapshot(&root, selector, max_results)?;
    serde_json::to_value(hits).map_err(|e| ServiceError::Internal(e.to_string()))
}

fn dispatch_act(ops: &mut dyn UiActorOps, cmd: ActCommand) -> Result<Value, ServiceError> {
    match cmd {
        ActCommand::LaunchApp {
            target,
            wait,
            timeout_ms,
        } => ops.launch_app(&target, wait.into(), Duration::from_millis(timeout_ms)),
        ActCommand::FocusApp {
            target,
            window_index,
        } => ops.focus_app(&target, window_index),
        ActCommand::WindowState { target, state } => ops.window_state(&target, state),
        ActCommand::WindowBounds {
            target,
            x,
            y,
            width,
            height,
        } => ops.window_bounds(&target, x, y, width, height),
        ActCommand::GetWindowBounds { target } => ops.get_window_bounds(&target),
        ActCommand::WindowScreenshot { target, path } => {
            ops.window_screenshot(&target, path.as_deref())
        }
        ActCommand::RunSteps {
            window,
            steps,
            reset,
            timeout_secs,
        } => ops.run_steps(&window, &steps, reset, Duration::from_secs_f64(timeout_secs.max(0.1))),
        ActCommand::Invoke {
            window,
            selector,
            timeout_secs,
        } => {
            let step = single_action_step(&selector, SingleStepKind::Invoke);
            ops.run_steps(&window, &[step], false, Duration::from_secs_f64(timeout_secs.max(0.1)))
        }
        ActCommand::SetValue {
            window,
            selector,
            value,
            timeout_secs,
        } => {
            let step = single_action_step(&selector, SingleStepKind::SetValue(&value));
            ops.run_steps(&window, &[step], false, Duration::from_secs_f64(timeout_secs.max(0.1)))
        }
        ActCommand::ClickSelector {
            window,
            selector,
            button,
            timeout_secs,
        } => {
            let step = single_action_step(&selector, SingleStepKind::Click(button));
            ops.run_steps(&window, &[step], false, Duration::from_secs_f64(timeout_secs.max(0.1)))
        }
        ActCommand::ListInstalledApps { .. } | ActCommand::KillApp { .. } => {
            unreachable!("handled directly on IO pool")
        }
    }
}

fn execute_list_installed_apps(filter: Option<&str>) -> Result<Value, ServiceError> {
    let mut apps = automata_linux::list_installed_apps();
    if let Some(f) = filter.map(|s| s.to_lowercase()) {
        apps.retain(|a| {
            a.id.to_lowercase().contains(&f)
                || a.name.to_lowercase().contains(&f)
                || a.exec.to_lowercase().contains(&f)
        });
    }
    serde_json::to_value(apps).map_err(|e| ServiceError::Internal(e.to_string()))
}

fn execute_kill_app(pid: Option<u32>, process: Option<&str>) -> Result<Value, ServiceError> {
    if let Some(p) = pid {
        automata_linux::process::kill_process_by_pid(p)?;
        return Ok(json!({ "killed_pids": [p] }));
    }
    if let Some(name) = process {
        let killed = automata_linux::process::kill_processes_by_name(name)?;
        return Ok(json!({ "killed_pids": killed }));
    }
    Err(ServiceError::InvalidInput(
        "app kill requires 'pid' or 'process'".into(),
    ))
}

async fn run_blocking_pool<F>(op: F) -> Result<CommandResult, ServiceError>
where
    F: FnOnce() -> Result<CommandResult, ServiceError> + Send + 'static,
{
    tokio::task::spawn_blocking(op)
        .await
        .map_err(|e| ServiceError::Internal(format!("blocking pool task failed: {e}")))?
}

fn fallback_caps(detail: String) -> CapabilitiesSnapshot {
    CapabilitiesSnapshot {
        display_backend: "unknown".into(),
        compositor: "unknown".into(),
        pointer_backend: None,
        keyboard_backend: None,
        clipboard_backend: None,
        screenshot_backend: None,
        ocr_backend: None,
        atspi_connected: false,
        list_windows: false,
        tree: false,
        window_bounds: false,
        window_stacking: false,
        window_focus: false,
        window_close: false,
        window_move_resize: false,
        input_pointer: false,
        input_keyboard: false,
        background_input: false,
        capture: false,
        ocr: false,
        browser_cdp: false,
        detail,
    }
}

/// Ensures `ServiceConfig` paths exist as needed and builds a config rooted under `base`.
pub fn temp_service_config(base: PathBuf, allow_file_writes: bool) -> ServiceConfig {
    ServiceConfig {
        connection_mode: ConnectionLifecycle::OneShot,
        workflows_dir: base.join("workflows"),
        logs_dir: base.join("logs"),
        allow_file_writes,
    }
}

#[allow(dead_code)]
fn _assert_coord_space_used(_: CoordSpace) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{
        BatchProbeStep, BrowserCommand, SystemCommand, Target, VisionCommand, WindowFilter,
        WindowStateArg,
    };
    use automata_linux::coords::WindowId;
    use std::sync::atomic::AtomicUsize;

    struct FakeUiActor {
        steps_calls: usize,
        reset_count: Arc<AtomicUsize>,
    }

    impl UiActorOps for FakeUiActor {
        fn capabilities(&self) -> CapabilitiesSnapshot {
            fallback_caps("fake".into())
        }
        fn reset_shadow_dom(&mut self) {
            self.reset_count.fetch_add(1, Ordering::Relaxed);
        }
        fn resolve_window_bounds(&mut self, _: WindowId) -> Result<(i32, i32, i32, i32), ServiceError> {
            Ok((100, 200, 800, 600))
        }
        fn list_windows(&mut self, _: &WindowFilter) -> Result<Value, ServiceError> {
            Ok(json!([{"hwnd": "0x100", "title": "FakeApp", "process": "fake"}]))
        }
        fn focused_window(&mut self) -> Result<Value, ServiceError> {
            Ok(json!({"hwnd": "0x100", "title": "FakeApp"}))
        }
        fn window_tree(&mut self, _: &Target, _: usize) -> Result<Value, ServiceError> {
            Ok(json!({"role": "window", "name": "FakeApp", "children": []}))
        }
        fn active_window_tree(&mut self, _: Option<&str>, _: usize) -> Result<Value, ServiceError> {
            Ok(json!({"role": "window", "name": "FakeApp"}))
        }
        fn find_elements(
            &mut self,
            _: &Target,
            selector: &str,
            _: bool,
            _: bool,
            _: usize,
        ) -> Result<Value, ServiceError> {
            Ok(json!([{"name": "OK", "selector": selector}]))
        }
        fn capture_bundle(
            &mut self,
            _: &Target,
            out_dir: &Path,
            _: usize,
            _: usize,
            _: bool,
        ) -> Result<Value, ServiceError> {
            std::fs::create_dir_all(out_dir)?;
            let root = ElementNode {
                role: "window".into(),
                name: "FakeApp".into(),
                automation_id: None,
                text: None,
                x: 100,
                y: 200,
                width: 800,
                height: 600,
                children: vec![ElementNode {
                    role: "button".into(),
                    name: "OK".into(),
                    automation_id: Some("ok_btn".into()),
                    text: None,
                    x: 120,
                    y: 240,
                    width: 80,
                    height: 30,
                    children: vec![],
                }],
            };
            let tree_path = out_dir.join("tree.json");
            std::fs::write(&tree_path, serde_json::to_vec(&root).unwrap())?;
            Ok(json!({
                "ok": true,
                "bundle_dir": out_dir.display().to_string(),
                "tree_path": tree_path.display().to_string(),
                "node_count": 2,
            }))
        }
        fn batch_explore(
            &mut self,
            _: &Target,
            steps: &[BatchProbeStep],
            stop_on_error: bool,
            bundle_dir: Option<&Path>,
        ) -> Result<Value, ServiceError> {
            let mut out = Vec::new();
            let mut failed = 0usize;
            for (idx, item) in steps.iter().enumerate() {
                let fail = item.name == "fail_me";
                if fail {
                    failed += 1;
                }
                out.push(json!({
                    "index": idx,
                    "name": item.name,
                    "ok": !fail,
                    "error": if fail { Some("simulated probe error") } else { None },
                }));
                if fail && stop_on_error {
                    break;
                }
            }
            Ok(json!({
                "ok": failed == 0,
                "total": steps.len(),
                "executed": out.len(),
                "succeeded": out.len() - failed,
                "failed": failed,
                "bundle_dir": bundle_dir.map(|p| p.display().to_string()),
                "steps": out,
            }))
        }
        fn vision(&mut self, _: &VisionCommand) -> Result<Value, ServiceError> {
            Ok(json!({"blocks": []}))
        }
        fn launch_app(&mut self, target: &str, _: ui_automata::LaunchWait, _: Duration) -> Result<Value, ServiceError> {
            Ok(json!({"pid": 999, "target": target}))
        }
        fn focus_app(&mut self, _: &Target, _: usize) -> Result<Value, ServiceError> {
            Ok(json!({"focused": true}))
        }
        fn window_state(&mut self, _: &Target, _: WindowStateArg) -> Result<Value, ServiceError> {
            Ok(json!({"ok": true}))
        }
        fn window_bounds(
            &mut self,
            _: &Target,
            x: Option<i32>,
            y: Option<i32>,
            w: Option<i32>,
            h: Option<i32>,
        ) -> Result<Value, ServiceError> {
            Ok(json!({"ok": true, "x": x, "y": y, "width": w, "height": h}))
        }
        fn get_window_bounds(&mut self, _: &Target) -> Result<Value, ServiceError> {
            Ok(json!({"x": 100, "y": 200, "width": 800, "height": 600}))
        }
        fn window_screenshot(&mut self, _: &Target, path: Option<&str>) -> Result<Value, ServiceError> {
            Ok(json!({"path": path.unwrap_or("/tmp/fake.png")}))
        }
        fn run_steps(&mut self, _: &Target, steps: &[Value], _: bool, _: Duration) -> Result<Value, ServiceError> {
            self.steps_calls += 1;
            Ok(json!({"ok": true, "steps_run": steps.len(), "calls": self.steps_calls}))
        }
        fn browser(&mut self, _: &BrowserCommand) -> Result<Value, ServiceError> {
            Ok(json!({"ok": true}))
        }
    }

    #[test]
    fn service_shares_session_records_yaml_and_enforces_file_policy() {
        let tmp = std::env::temp_dir().join(format!("automata_service_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("workflows")).unwrap();
        let reset_count = Arc::new(AtomicUsize::new(0));
        let reset_clone = reset_count.clone();

        let svc = AutomationService::open_with_actor(temp_service_config(tmp.clone(), false), move || {
            Box::new(FakeUiActor {
                steps_calls: 0,
                reset_count: reset_clone,
            })
        });

        let mut events_rx = svc.events();
        let ping = svc.call(Command::System(SystemCommand::Ping)).unwrap();
        assert_eq!(ping.data["pong"], true);

        let wins = svc
            .call(Command::Explore(ExploreCommand::ListWindows {
                filter: Default::default(),
            }))
            .unwrap();
        assert_eq!(wins.data.as_array().unwrap().len(), 1);

        // Policy blocks file writes when allow_file_writes == false.
        let write_err = svc
            .call(Command::File(FileCommand::Raw(json!({
                "action": "write",
                "path": tmp.join("blocked.txt"),
                "content": "no"
            }))))
            .unwrap_err();
        assert!(matches!(write_err, ServiceError::PermissionDenied(_)));

        // Start recording -> execute semantic act -> export validated YAML.
        svc.call(Command::Session(SessionCommand::RecordStart {
            name: "contract_rec".into(),
            target: Some(Target::process("fake")),
        }))
        .unwrap();

        svc.call(Command::Act(ActCommand::Invoke {
            window: Target::process("fake"),
            selector: ">> [role=button][name=OK]".into(),
            timeout_secs: 2.0,
        }))
        .unwrap();

        let exported = svc
            .call(Command::Session(SessionCommand::RecordStop {
                output_path: None,
            }))
            .unwrap();
        assert_eq!(exported.data["valid"], true);
        assert_eq!(exported.data["steps_count"], 1);

        svc.call(Command::Session(SessionCommand::ResetShadowDom)).unwrap();
        assert_eq!(reset_count.load(Ordering::Relaxed), 1);

        let mut saw_finished = false;
        while let Ok(evt) = events_rx.try_recv() {
            if matches!(evt, Event::CommandFinished { ok: true, .. }) {
                saw_finished = true;
            }
        }
        assert!(saw_finished);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn async_submit_and_cancel_workflow_work_cleanly() {
        let tmp = std::env::temp_dir().join(format!("automata_service_async_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("workflows")).unwrap();
        let reset_count = Arc::new(AtomicUsize::new(0));
        let svc = AutomationService::open_with_actor(temp_service_config(tmp.clone(), true), move || {
            Box::new(FakeUiActor {
                steps_calls: 0,
                reset_count,
            })
        });

        let handle = svc.submit(Command::Explore(ExploreCommand::FocusedWindow));
        let res = handle.wait_async().await.unwrap();
        assert_eq!(res.data["title"], "FakeApp");

        // Lint invalid yaml cleanly
        let lint = svc
            .call_async(Command::Workflow(WorkflowCommand::Lint {
                yaml: Some("not: [valid".into()),
                file: None,
            }))
            .await
            .unwrap();
        assert_eq!(lint.data["valid"], false);

        // CaptureBundle + offline QuerySnapshot + partial-error Batch contract
        let bundle_dir = tmp.join("snap_bundle");
        let cap = svc
            .explore()
            .capture_bundle(Target::process("fake"), Some(bundle_dir.clone()), false)
            .unwrap();
        assert_eq!(cap.data["ok"], true);
        let offline = svc
            .explore()
            .query_snapshot(bundle_dir.clone(), ">> [role=button][id=ok_btn]", 10)
            .unwrap();
        assert_eq!(offline.data.as_array().unwrap().len(), 1);
        assert_eq!(offline.data[0]["name"], "OK");

        let batch = svc
            .explore()
            .batch(
                Target::process("fake"),
                vec![
                    BatchProbeStep {
                        name: "fail_me".into(),
                        step: None,
                        settle_ms: 0,
                        capture_tree: false,
                        capture_screenshot: false,
                        capture_ocr: false,
                        query: None,
                        max_results: 10,
                    },
                    BatchProbeStep {
                        name: "after_fail".into(),
                        step: None,
                        settle_ms: 0,
                        capture_tree: false,
                        capture_screenshot: false,
                        capture_ocr: false,
                        query: None,
                        max_results: 10,
                    },
                ],
                false,
                None,
            )
            .unwrap();
        assert_eq!(batch.data["ok"], false);
        assert_eq!(batch.data["executed"], 2);
        assert_eq!(batch.data["succeeded"], 1);
        assert_eq!(batch.data["failed"], 1);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
