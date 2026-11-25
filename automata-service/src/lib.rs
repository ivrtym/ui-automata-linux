//! Unified session-aware automation service (`AutomationService`), strongly-typed
//! `Command` / `Event` protocol, ergonomic Rust APIs, local `0600` Unix socket
//! daemon, and workflow recorder.

pub mod actor;
pub mod api;
pub mod daemon;
pub mod error;
pub mod file_ops;
pub mod protocol;
pub mod recorder;
pub mod service;
pub mod session;
pub mod system_ops;

pub use error::ServiceError;
pub use protocol::{
    ActCommand, BatchProbeStep, BrowserCommand, ClickButtonArg, ClipboardCommand, Command,
    CommandResult, ConnectionLifecycle, Event, ExploreCommand, FileCommand, InputCommand,
    LaunchWaitArg, ServiceConfig, ServiceInfo, SessionCommand, SystemCommand, Target,
    VisionCommand, WindowFilter, WindowStateArg, WorkflowCommand,
};
pub use service::{AutomationService, CommandHandle};
