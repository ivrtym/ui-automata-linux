//! Strongly-typed Command / Event / Session / Config protocol shared across
//! Rust callers, `automata` CLI/daemon, and `automata-agent` MCP.

use std::collections::HashMap;
use std::path::PathBuf;

use automata_linux::coords::{SpacePoint, SpaceRect, WindowId};
use automata_linux::diagnostics::CapabilitiesSnapshot;
use automata_linux::{ConnectionMode, WindowAction};
use serde::{Deserialize, Serialize};
use ui_automata::{ClickType, LaunchWait};

/// Serializable connection lifecycle preference for [`ServiceConfig`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionLifecycle {
    OneShot,
    #[default]
    Persistent,
}

impl From<ConnectionLifecycle> for ConnectionMode {
    fn from(value: ConnectionLifecycle) -> Self {
        match value {
            ConnectionLifecycle::OneShot => ConnectionMode::OneShot,
            ConnectionLifecycle::Persistent => ConnectionMode::persistent_default(),
        }
    }
}

impl From<ConnectionMode> for ConnectionLifecycle {
    fn from(value: ConnectionMode) -> Self {
        match value {
            ConnectionMode::OneShot => ConnectionLifecycle::OneShot,
            ConnectionMode::Persistent { .. } => ConnectionLifecycle::Persistent,
        }
    }
}

/// Configuration used when opening an in-process [`crate::AutomationService`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceConfig {
    pub connection_mode: ConnectionLifecycle,
    pub workflows_dir: PathBuf,
    pub logs_dir: PathBuf,
    pub allow_file_writes: bool,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        let base = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join(".ui-automata");
        Self {
            connection_mode: ConnectionLifecycle::Persistent,
            workflows_dir: base.join("workflows"),
            logs_dir: base.join("logs"),
            allow_file_writes: true,
        }
    }
}

/// Filter used when enumerating windows.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowFilter {
    #[serde(default)]
    pub process: Option<String>,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub automation_id: Option<String>,
    #[serde(default)]
    pub window_id: Option<WindowId>,
}

/// Target window / element selector passed to `Explore`, `Act`, and `Recorder`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    #[serde(default)]
    pub process: Option<String>,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub automation_id: Option<String>,
    #[serde(default)]
    pub selector: Option<String>,
    #[serde(default)]
    pub window_id: Option<WindowId>,
}

impl Target {
    pub fn process(name: impl Into<String>) -> Self {
        Self {
            process: Some(name.into()),
            ..Default::default()
        }
    }

    pub fn window(id: WindowId) -> Self {
        Self {
            window_id: Some(id),
            ..Default::default()
        }
    }

    pub fn selector(selector: impl Into<String>) -> Self {
        Self {
            selector: Some(selector.into()),
            ..Default::default()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.process.is_none()
            && self.pid.is_none()
            && self.title.is_none()
            && self.automation_id.is_none()
            && self.selector.is_none()
            && self.window_id.is_none()
    }
}

/// Unified top-level command enum executed by `AutomationService::call`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "group", content = "payload", rename_all = "snake_case")]
pub enum Command {
    Session(SessionCommand),
    Explore(ExploreCommand),
    Act(ActCommand),
    Vision(VisionCommand),
    Workflow(WorkflowCommand),
    Input(InputCommand),
    Clipboard(ClipboardCommand),
    Browser(BrowserCommand),
    File(FileCommand),
    System(SystemCommand),
}

impl Command {
    pub fn kind_label(&self) -> &'static str {
        match self {
            Self::Session(_) => "session",
            Self::Explore(_) => "explore",
            Self::Act(_) => "act",
            Self::Vision(_) => "vision",
            Self::Workflow(_) => "workflow",
            Self::Input(_) => "input",
            Self::Clipboard(_) => "clipboard",
            Self::Browser(_) => "browser",
            Self::File(_) => "file",
            Self::System(_) => "system",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum SessionCommand {
    Info,
    Capabilities,
    ResetShadowDom,
    RecordStart {
        name: String,
        #[serde(default)]
        target: Option<Target>,
    },
    RecordStatus,
    RecordStop {
        #[serde(default)]
        output_path: Option<PathBuf>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchProbeStep {
    pub name: String,
    #[serde(default)]
    pub step: Option<serde_json::Value>,
    #[serde(default)]
    pub settle_ms: u64,
    #[serde(default)]
    pub capture_tree: bool,
    #[serde(default)]
    pub capture_screenshot: bool,
    #[serde(default)]
    pub capture_ocr: bool,
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default = "default_find_max_results")]
    pub max_results: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ExploreCommand {
    ListWindows {
        #[serde(default)]
        filter: WindowFilter,
    },
    FocusedWindow,
    WindowTree {
        target: Target,
        #[serde(default = "default_tree_max_results")]
        max_results: usize,
    },
    ActiveWindowTree {
        #[serde(default)]
        selector: Option<String>,
        #[serde(default = "default_tree_max_results")]
        max_results: usize,
    },
    FindElements {
        target: Target,
        selector: String,
        #[serde(default = "default_true")]
        include_ancestors: bool,
        #[serde(default)]
        include_siblings: bool,
        #[serde(default = "default_find_max_results")]
        max_results: usize,
    },
    CaptureBundle {
        target: Target,
        #[serde(default)]
        out_dir: Option<PathBuf>,
        #[serde(default = "default_bundle_max_nodes")]
        max_nodes: usize,
        #[serde(default = "default_bundle_max_depth")]
        max_depth: usize,
        #[serde(default)]
        include_ocr: bool,
    },
    QuerySnapshot {
        bundle_dir: PathBuf,
        selector: String,
        #[serde(default = "default_find_max_results")]
        max_results: usize,
    },
    Batch {
        target: Target,
        steps: Vec<BatchProbeStep>,
        #[serde(default)]
        stop_on_error: bool,
        #[serde(default)]
        bundle_dir: Option<PathBuf>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ActCommand {
    LaunchApp {
        target: String,
        #[serde(default)]
        wait: LaunchWaitArg,
        #[serde(default = "default_launch_timeout_ms")]
        timeout_ms: u64,
    },
    ListInstalledApps {
        #[serde(default)]
        filter: Option<String>,
    },
    FocusApp {
        target: Target,
        #[serde(default)]
        window_index: usize,
    },
    KillApp {
        #[serde(default)]
        pid: Option<u32>,
        #[serde(default)]
        process: Option<String>,
    },
    WindowState {
        target: Target,
        state: WindowStateArg,
    },
    WindowBounds {
        target: Target,
        #[serde(default)]
        x: Option<i32>,
        #[serde(default)]
        y: Option<i32>,
        #[serde(default)]
        width: Option<i32>,
        #[serde(default)]
        height: Option<i32>,
    },
    GetWindowBounds {
        target: Target,
    },
    WindowScreenshot {
        target: Target,
        #[serde(default)]
        path: Option<String>,
    },
    /// Run a declarative batch of step definitions sharing persistent `ShadowDom`.
    RunSteps {
        window: Target,
        steps: Vec<serde_json::Value>,
        #[serde(default)]
        reset: bool,
        #[serde(default = "default_step_timeout_secs")]
        timeout_secs: f64,
    },
    /// Single ergonomic semantic invoke executed through the shared `ShadowDom`.
    Invoke {
        window: Target,
        selector: String,
        #[serde(default = "default_step_timeout_secs")]
        timeout_secs: f64,
    },
    /// Single ergonomic AT-SPI `SetValue` executed through the shared `ShadowDom`.
    SetValue {
        window: Target,
        selector: String,
        value: String,
        #[serde(default = "default_step_timeout_secs")]
        timeout_secs: f64,
    },
    /// Single ergonomic element click executed through the shared `ShadowDom`.
    ClickSelector {
        window: Target,
        selector: String,
        #[serde(default)]
        button: ClickButtonArg,
        #[serde(default = "default_step_timeout_secs")]
        timeout_secs: f64,
    },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaunchWaitArg {
    #[default]
    NewPid,
    NewWindow,
    MatchAny,
}

impl From<LaunchWaitArg> for LaunchWait {
    fn from(value: LaunchWaitArg) -> Self {
        match value {
            LaunchWaitArg::NewPid => LaunchWait::NewPid,
            LaunchWaitArg::NewWindow => LaunchWait::NewWindow,
            LaunchWaitArg::MatchAny => LaunchWait::MatchAny,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowStateArg {
    Minimize,
    Maximize,
    Restore,
    Close,
    Focus,
}

impl TryFrom<WindowStateArg> for WindowAction {
    type Error = ();
    fn try_from(value: WindowStateArg) -> Result<Self, Self::Error> {
        match value {
            WindowStateArg::Minimize => Ok(WindowAction::Minimize),
            WindowStateArg::Maximize => Ok(WindowAction::Maximize),
            WindowStateArg::Restore => Ok(WindowAction::Restore),
            WindowStateArg::Close => Ok(WindowAction::Close),
            WindowStateArg::Focus => Err(()),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClickButtonArg {
    #[default]
    Left,
    Right,
    Middle,
    Double,
    Triple,
}

impl From<ClickButtonArg> for ClickType {
    fn from(value: ClickButtonArg) -> Self {
        match value {
            ClickButtonArg::Left => ClickType::Left,
            ClickButtonArg::Right => ClickType::Right,
            ClickButtonArg::Middle => ClickType::Middle,
            ClickButtonArg::Double => ClickType::Double,
            ClickButtonArg::Triple => ClickType::Triple,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum VisionCommand {
    WindowLayout {
        window_id: WindowId,
        #[serde(default)]
        capture_only: bool,
        #[serde(default)]
        save_screenshot: bool,
    },
    WindowProbe {
        window_id: WindowId,
        #[serde(default)]
        capture_only: bool,
        #[serde(default)]
        save_screenshot: bool,
    },
    ScreenLayout {
        #[serde(default)]
        capture_only: bool,
        #[serde(default)]
        save_screenshot: bool,
    },
    Capture {
        #[serde(default)]
        window_id: Option<WindowId>,
        #[serde(default)]
        region: Option<SpaceRect>,
        #[serde(default)]
        path: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum InputCommand {
    MouseMove {
        point: SpacePoint,
    },
    MouseClick {
        point: SpacePoint,
        #[serde(default)]
        button: ClickButtonArg,
    },
    MouseDrag {
        from: SpacePoint,
        to: SpacePoint,
    },
    MouseScroll {
        point: SpacePoint,
        #[serde(default)]
        delta_x: i32,
        #[serde(default)]
        delta_y: i32,
    },
    KeyPress {
        keys: String,
    },
    TypeText {
        text: String,
    },
    GetCursorPos,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ClipboardCommand {
    Read,
    Write { text: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum BrowserCommand {
    Ensure,
    Tabs,
    Open {
        #[serde(default)]
        url: Option<String>,
    },
    Activate {
        tab_id: String,
    },
    Close {
        tab_id: String,
    },
    Navigate {
        tab_id: String,
        url: String,
    },
    Dom {
        tab_id: String,
    },
    Eval {
        tab_id: String,
        expression: String,
    },
    Screenshot {
        tab_id: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum FileCommand {
    /// Delegates any structured JSON file tool request while enforcing `allow_file_writes`.
    Raw(serde_json::Value),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum SystemCommand {
    Ping,
    Whoami,
    Hostname,
    Env,
    GetPath,
    IpConfig,
    RouteTable,
    ListProcesses,
    KillProcess {
        #[serde(default)]
        pid: Option<u32>,
        #[serde(default)]
        process_name: Option<String>,
    },
    Exec {
        args: Vec<String>,
        #[serde(default = "default_exec_timeout_secs")]
        timeout_secs: u64,
    },
    Doctor {
        #[serde(default)]
        profile: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum WorkflowCommand {
    List,
    Read {
        name: String,
    },
    Lint {
        #[serde(default)]
        yaml: Option<String>,
        #[serde(default)]
        file: Option<String>,
    },
    RunFile {
        file: String,
        #[serde(default)]
        params: HashMap<String, String>,
        #[serde(default)]
        background: bool,
    },
    RunInline {
        yaml: String,
        #[serde(default)]
        params: HashMap<String, String>,
        #[serde(default)]
        background: bool,
    },
    Status,
    Cancel,
    ListRuns {
        #[serde(default = "default_find_max_results")]
        limit: usize,
    },
    ListResources,
    ReadResource {
        name: String,
    },
}

/// Unified command response carrying JSON-serializable structured output.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CommandResult {
    pub data: serde_json::Value,
}

impl CommandResult {
    pub fn from_value(data: serde_json::Value) -> Self {
        Self { data }
    }

    pub fn serialize_result<T: Serialize>(val: &T) -> Result<Self, crate::ServiceError> {
        serde_json::to_value(val)
            .map(|data| Self { data })
            .map_err(|e| crate::ServiceError::Internal(e.to_string()))
    }

    pub fn into_value(self) -> serde_json::Value {
        self.data
    }
}

/// Snapshot of runtime service status and platform capabilities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceInfo {
    pub version: String,
    pub connection_mode: ConnectionLifecycle,
    pub workflows_dir: String,
    pub logs_dir: String,
    pub allow_file_writes: bool,
    pub recording: bool,
    pub workflow_running: bool,
    pub capabilities: CapabilitiesSnapshot,
}

/// Service event stream payload emitted on command start/completion, workflow phases, and recording.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    CommandStarted {
        command_id: u64,
        kind: String,
    },
    CommandFinished {
        command_id: u64,
        kind: String,
        ok: bool,
    },
    PhaseStarted {
        workflow: String,
        phase: String,
        completed: usize,
        total: usize,
    },
    PhaseCompleted {
        workflow: String,
        phase: String,
        completed: usize,
        total: usize,
        skipped: bool,
    },
    PhaseFailed {
        workflow: String,
        phase: String,
        error: String,
    },
    WorkflowFinished {
        workflow: String,
        ok: bool,
        duration_ms: u64,
    },
    RecorderUpdated {
        recording: bool,
        steps_count: usize,
    },
}

fn default_true() -> bool {
    true
}
fn default_tree_max_results() -> usize {
    10
}
fn default_find_max_results() -> usize {
    20
}
fn default_bundle_max_nodes() -> usize {
    5_000
}
fn default_bundle_max_depth() -> usize {
    32
}
fn default_launch_timeout_ms() -> u64 {
    30_000
}
fn default_step_timeout_secs() -> f64 {
    10.0
}
fn default_exec_timeout_secs() -> u64 {
    60
}
