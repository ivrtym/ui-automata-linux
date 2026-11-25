pub type Result<T> = anyhow::Result<T>;

/// Window state action for [`set_window_state`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowAction {
    Minimize,
    Maximize,
    Restore,
    Close,
}

pub mod a11y;
pub mod backend;
mod browser;
mod capture;
mod clipboard;
pub mod compositor;
pub mod coords;
mod desktop;
mod dialog;
pub mod diagnostics;
pub mod doctor;
mod error;
pub mod input;
mod locator;
pub mod mouse;
pub mod process;
pub mod runtime;
mod selector;
pub mod vision;
pub mod vision_omniparser;
#[cfg(feature = "ocr-ort")]
mod vision_ort;
pub mod window;
mod windows_info;

pub use a11y::*;
pub use backend::*;
pub use browser::*;
pub use capture::{CaptureSessionGuard, intersect_rect, monitors_have_gap};
pub use clipboard::*;
pub use coords::*;
pub use desktop::*;
pub use dialog::*;
pub use doctor::*;
pub use error::*;
pub use locator::*;
pub use mouse::*;
pub use selector::*;
pub use vision::*;
pub use window::*;
pub use windows_info::*;

/// Run a UI-Automata YAML workflow from an in-memory string.
pub fn run_workflow_str(
    yaml: &str,
    params: &std::collections::HashMap<String, String>,
) -> Result<()> {
    let workflow =
        ui_automata::yaml::WorkflowFile::load_from_str(yaml, params).map_err(anyhow::Error::msg)?;
    let desktop = Desktop::new();
    let mut executor = ui_automata::Executor::new(desktop);
    log::info!("running workflow '{}' ...", workflow.name);
    workflow
        .run(&mut executor, None, None)
        .map(|_| ())
        .map_err(anyhow::Error::msg)
}

/// Send `SIGINT` to a process by PID. Returns `true` on success.
pub fn send_ctrl_c(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-INT", &pid.to_string()])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

const OWN_CRATES: &[&str] = &["automata_browser", "automata_linux", "ui_automata"];

fn make_file_dispatch(log_path: &std::path::Path) -> Option<fern::Dispatch> {
    match fern::log_file(log_path) {
        Ok(file) => {
            let mut dispatch = fern::Dispatch::new()
                .level(log::LevelFilter::Info)
                .format(|out, message, record| {
                    out.finish(format_args!(
                        "[{}] [{}] {}",
                        chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ"),
                        record.level(),
                        message
                    ))
                })
                .chain(file);
            for krate in OWN_CRATES {
                dispatch = dispatch.level_for(*krate, log::LevelFilter::Debug);
            }
            Some(dispatch)
        }
        Err(e) => {
            eprintln!("could not open log file {}: {e}", log_path.display());
            None
        }
    }
}

/// File-only logger — no stdout output (used in pipe mode).
pub fn init_logging_file_only(log_path: &std::path::Path) {
    let mut dispatch = fern::Dispatch::new();
    if let Some(file_dispatch) = make_file_dispatch(log_path) {
        dispatch = dispatch.chain(file_dispatch);
    }
    if let Err(e) = dispatch.apply() {
        eprintln!("failed to initialise logging: {e}");
    }
}

/// Initialise logging: coloured stdout + optional log file.
pub fn init_logging(log_path: Option<&std::path::Path>) {
    let stdout_level = std::env::var("RUST_LOG")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(log::LevelFilter::Info);

    let colors = fern::colors::ColoredLevelConfig::new()
        .error(fern::colors::Color::Red)
        .warn(fern::colors::Color::Yellow)
        .info(fern::colors::Color::Green)
        .debug(fern::colors::Color::Cyan)
        .trace(fern::colors::Color::White);

    let stdout_dispatch = fern::Dispatch::new()
        .level(stdout_level)
        .format(move |out, message, record| {
            out.finish(format_args!(
                "[{} {:<5} {}] {}",
                chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ"),
                colors.color(record.level()),
                record.target(),
                message
            ))
        })
        .chain(std::io::stdout());

    let mut dispatch = fern::Dispatch::new().chain(stdout_dispatch);

    if let Some(path) = log_path {
        if let Some(file_dispatch) = make_file_dispatch(path) {
            dispatch = dispatch.chain(file_dispatch);
        }
    }

    if let Err(e) = dispatch.apply() {
        eprintln!("failed to initialise logging: {e}");
    }
}

/// Initialise AT-SPI2 connection eagerly (optional; `A11yClient::shared()` connects on demand).
pub fn init_a11y() {
    if let Err(e) = A11yClient::shared() {
        log::warn!("AT-SPI2 initialization warning: {e}");
    }
}
