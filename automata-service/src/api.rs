//! Ergonomic fluent Rust API facades over [`AutomationService::call`] (`M3.3`).

use std::collections::HashMap;
use std::path::PathBuf;

use automata_linux::coords::{SpacePoint, SpaceRect, WindowId};

use crate::protocol::{
    ActCommand, BatchProbeStep, ClickButtonArg, ClipboardCommand, Command, CommandResult,
    ExploreCommand, FileCommand, InputCommand, LaunchWaitArg, SessionCommand, SystemCommand,
    Target, VisionCommand, WindowFilter, WindowStateArg, WorkflowCommand,
};
use crate::{AutomationService, ServiceError};

impl AutomationService {
    pub fn session(&self) -> SessionApi<'_> {
        SessionApi(self)
    }

    pub fn explore(&self) -> ExploreApi<'_> {
        ExploreApi(self)
    }

    pub fn act(&self) -> ActApi<'_> {
        ActApi(self)
    }

    pub fn vision(&self) -> VisionApi<'_> {
        VisionApi(self)
    }

    pub fn workflow(&self) -> WorkflowApi<'_> {
        WorkflowApi(self)
    }

    pub fn input(&self) -> InputApi<'_> {
        InputApi(self)
    }

    pub fn clipboard(&self) -> ClipboardApi<'_> {
        ClipboardApi(self)
    }

    pub fn system(&self) -> SystemApi<'_> {
        SystemApi(self)
    }

    pub fn file(&self) -> FileApi<'_> {
        FileApi(self)
    }
}

pub struct SessionApi<'a>(&'a AutomationService);

impl SessionApi<'_> {
    pub fn info(&self) -> Result<CommandResult, ServiceError> {
        self.0.call(Command::Session(SessionCommand::Info))
    }

    pub fn capabilities(&self) -> Result<CommandResult, ServiceError> {
        self.0.call(Command::Session(SessionCommand::Capabilities))
    }

    pub fn reset_shadow_dom(&self) -> Result<CommandResult, ServiceError> {
        self.0.call(Command::Session(SessionCommand::ResetShadowDom))
    }

    pub fn record_start(
        &self,
        name: impl Into<String>,
        target: Option<Target>,
    ) -> Result<CommandResult, ServiceError> {
        self.0.call(Command::Session(SessionCommand::RecordStart {
            name: name.into(),
            target,
        }))
    }

    pub fn record_status(&self) -> Result<CommandResult, ServiceError> {
        self.0.call(Command::Session(SessionCommand::RecordStatus))
    }

    pub fn record_stop(&self, output_path: Option<PathBuf>) -> Result<CommandResult, ServiceError> {
        self.0
            .call(Command::Session(SessionCommand::RecordStop { output_path }))
    }
}

pub struct ExploreApi<'a>(&'a AutomationService);

impl ExploreApi<'_> {
    pub fn list_windows(&self, filter: WindowFilter) -> Result<CommandResult, ServiceError> {
        self.0
            .call(Command::Explore(ExploreCommand::ListWindows { filter }))
    }

    pub fn focused_window(&self) -> Result<CommandResult, ServiceError> {
        self.0.call(Command::Explore(ExploreCommand::FocusedWindow))
    }

    pub fn window_tree(
        &self,
        target: Target,
        max_results: usize,
    ) -> Result<CommandResult, ServiceError> {
        self.0.call(Command::Explore(ExploreCommand::WindowTree {
            target,
            max_results,
        }))
    }

    pub fn find_elements(
        &self,
        target: Target,
        selector: impl Into<String>,
        max_results: usize,
    ) -> Result<CommandResult, ServiceError> {
        self.0.call(Command::Explore(ExploreCommand::FindElements {
            target,
            selector: selector.into(),
            include_ancestors: true,
            include_siblings: false,
            max_results,
        }))
    }

    pub fn capture_bundle(
        &self,
        target: Target,
        out_dir: Option<PathBuf>,
        include_ocr: bool,
    ) -> Result<CommandResult, ServiceError> {
        self.0.call(Command::Explore(ExploreCommand::CaptureBundle {
            target,
            out_dir,
            max_nodes: 5_000,
            max_depth: 32,
            include_ocr,
        }))
    }

    pub fn query_snapshot(
        &self,
        bundle_dir: PathBuf,
        selector: impl Into<String>,
        max_results: usize,
    ) -> Result<CommandResult, ServiceError> {
        self.0.call(Command::Explore(ExploreCommand::QuerySnapshot {
            bundle_dir,
            selector: selector.into(),
            max_results,
        }))
    }

    pub fn batch(
        &self,
        target: Target,
        steps: Vec<BatchProbeStep>,
        stop_on_error: bool,
        bundle_dir: Option<PathBuf>,
    ) -> Result<CommandResult, ServiceError> {
        self.0.call(Command::Explore(ExploreCommand::Batch {
            target,
            steps,
            stop_on_error,
            bundle_dir,
        }))
    }
}

pub struct ActApi<'a>(&'a AutomationService);

impl ActApi<'_> {
    pub fn launch(
        &self,
        target: impl Into<String>,
        wait: LaunchWaitArg,
        timeout_ms: u64,
    ) -> Result<CommandResult, ServiceError> {
        self.0.call(Command::Act(ActCommand::LaunchApp {
            target: target.into(),
            wait,
            timeout_ms,
        }))
    }

    pub fn invoke(
        &self,
        window: Target,
        selector: impl Into<String>,
    ) -> Result<CommandResult, ServiceError> {
        self.0.call(Command::Act(ActCommand::Invoke {
            window,
            selector: selector.into(),
            timeout_secs: 10.0,
        }))
    }

    pub fn set_value(
        &self,
        window: Target,
        selector: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<CommandResult, ServiceError> {
        self.0.call(Command::Act(ActCommand::SetValue {
            window,
            selector: selector.into(),
            value: value.into(),
            timeout_secs: 10.0,
        }))
    }

    pub fn click(
        &self,
        window: Target,
        selector: impl Into<String>,
        button: ClickButtonArg,
    ) -> Result<CommandResult, ServiceError> {
        self.0.call(Command::Act(ActCommand::ClickSelector {
            window,
            selector: selector.into(),
            button,
            timeout_secs: 10.0,
        }))
    }

    pub fn close_window(&self, target: Target) -> Result<CommandResult, ServiceError> {
        self.0.call(Command::Act(ActCommand::WindowState {
            target,
            state: WindowStateArg::Close,
        }))
    }

    pub fn run_steps(
        &self,
        window: Target,
        steps: Vec<serde_json::Value>,
    ) -> Result<CommandResult, ServiceError> {
        self.0.call(Command::Act(ActCommand::RunSteps {
            window,
            steps,
            reset: false,
            timeout_secs: 10.0,
        }))
    }
}

pub struct VisionApi<'a>(&'a AutomationService);

impl VisionApi<'_> {
    pub fn window_layout(&self, window_id: WindowId) -> Result<CommandResult, ServiceError> {
        self.0.call(Command::Vision(VisionCommand::WindowLayout {
            window_id,
            capture_only: false,
            save_screenshot: false,
        }))
    }

    pub fn capture(
        &self,
        window_id: Option<WindowId>,
        region: Option<SpaceRect>,
        path: Option<String>,
    ) -> Result<CommandResult, ServiceError> {
        self.0.call(Command::Vision(VisionCommand::Capture {
            window_id,
            region,
            path,
        }))
    }
}

pub struct WorkflowApi<'a>(&'a AutomationService);

impl WorkflowApi<'_> {
    pub fn list(&self) -> Result<CommandResult, ServiceError> {
        self.0.call(Command::Workflow(WorkflowCommand::List))
    }

    pub fn lint_yaml(&self, yaml: impl Into<String>) -> Result<CommandResult, ServiceError> {
        self.0.call(Command::Workflow(WorkflowCommand::Lint {
            yaml: Some(yaml.into()),
            file: None,
        }))
    }

    pub fn run_file(
        &self,
        file: impl Into<String>,
        params: HashMap<String, String>,
    ) -> Result<CommandResult, ServiceError> {
        self.0.call(Command::Workflow(WorkflowCommand::RunFile {
            file: file.into(),
            params,
            background: false,
        }))
    }

    pub fn run_inline(
        &self,
        yaml: impl Into<String>,
        params: HashMap<String, String>,
    ) -> Result<CommandResult, ServiceError> {
        self.0.call(Command::Workflow(WorkflowCommand::RunInline {
            yaml: yaml.into(),
            params,
            background: false,
        }))
    }

    pub fn status(&self) -> Result<CommandResult, ServiceError> {
        self.0.call(Command::Workflow(WorkflowCommand::Status))
    }

    pub fn cancel(&self) -> Result<CommandResult, ServiceError> {
        self.0.call(Command::Workflow(WorkflowCommand::Cancel))
    }
}

pub struct InputApi<'a>(&'a AutomationService);

impl InputApi<'_> {
    pub fn mouse_move(&self, point: SpacePoint) -> Result<CommandResult, ServiceError> {
        self.0.call(Command::Input(InputCommand::MouseMove { point }))
    }

    pub fn mouse_click(
        &self,
        point: SpacePoint,
        button: ClickButtonArg,
    ) -> Result<CommandResult, ServiceError> {
        self.0
            .call(Command::Input(InputCommand::MouseClick { point, button }))
    }

    pub fn key_press(&self, keys: impl Into<String>) -> Result<CommandResult, ServiceError> {
        self.0
            .call(Command::Input(InputCommand::KeyPress { keys: keys.into() }))
    }

    pub fn type_text(&self, text: impl Into<String>) -> Result<CommandResult, ServiceError> {
        self.0
            .call(Command::Input(InputCommand::TypeText { text: text.into() }))
    }
}

pub struct ClipboardApi<'a>(&'a AutomationService);

impl ClipboardApi<'_> {
    pub fn read(&self) -> Result<CommandResult, ServiceError> {
        self.0.call(Command::Clipboard(ClipboardCommand::Read))
    }

    pub fn write(&self, text: impl Into<String>) -> Result<CommandResult, ServiceError> {
        self.0
            .call(Command::Clipboard(ClipboardCommand::Write { text: text.into() }))
    }
}

pub struct SystemApi<'a>(&'a AutomationService);

impl SystemApi<'_> {
    pub fn ping(&self) -> Result<CommandResult, ServiceError> {
        self.0.call(Command::System(SystemCommand::Ping))
    }
}

pub struct FileApi<'a>(&'a AutomationService);

impl FileApi<'_> {
    pub fn raw(&self, request: serde_json::Value) -> Result<CommandResult, ServiceError> {
        self.0.call(Command::File(FileCommand::Raw(request)))
    }
}
