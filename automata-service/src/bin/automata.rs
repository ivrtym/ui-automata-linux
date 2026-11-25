//! Unified thin CLI, REPL, and local UDS daemon controller (`automata`).

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use automata_linux::coords::{SpacePoint, WindowId};
use clap::{Args, Parser, Subcommand};

use automata_service::daemon::{
    call_daemon, default_socket_path, default_systemd_unit_path, is_daemon_alive,
    render_systemd_user_unit, run_daemon_server, stop_daemon,
};
use automata_service::protocol::{
    ActCommand, BatchProbeStep, ClickButtonArg, Command, CommandResult, ConnectionLifecycle,
    ExploreCommand, InputCommand, LaunchWaitArg, ServiceConfig, SessionCommand, SystemCommand,
    Target, VisionCommand, WindowFilter, WindowStateArg, WorkflowCommand,
};
use automata_service::{AutomationService, ServiceError};

#[derive(Parser)]
#[command(name = "automata", version, about = "Unified Linux UI Automation CLI / REPL / UDS Daemon")]
struct Cli {
    /// Output machine-readable JSON
    #[arg(long, global = true)]
    json: bool,

    /// Custom Unix domain socket path for daemon mode
    #[arg(long, global = true)]
    socket: Option<PathBuf>,

    /// Force in-process execution even if a local daemon socket is active
    #[arg(long, global = true)]
    no_daemon: bool,

    /// Override workflows directory
    #[arg(long, global = true)]
    workflows_dir: Option<PathBuf>,

    #[command(subcommand)]
    cmd: TopCmd,
}

#[derive(Subcommand)]
enum TopCmd {
    /// Manage the local 0600 Unix socket daemon for shared CLI sessions
    Daemon {
        #[command(subcommand)]
        action: DaemonCmd,
    },
    /// Explore windows, accessibility trees, and selectors
    Explore {
        #[command(subcommand)]
        action: ExploreCmd,
    },
    /// Execute app launch, window control, or semantic/step actions
    Act {
        #[command(subcommand)]
        action: ActCmd,
    },
    /// Capture screenshots or inspect OCR layout
    Vision {
        #[command(subcommand)]
        action: VisionCmd,
    },
    /// List, lint, run, and monitor YAML workflows
    Workflow {
        #[command(subcommand)]
        action: WorkflowCmd,
    },
    /// Record executed actions and export validated YAML workflows
    Record {
        #[command(subcommand)]
        action: RecordCmd,
    },
    /// Run system ping, process inspection, or environment doctor
    System {
        #[command(subcommand)]
        action: SystemCmd,
    },
    /// Interactive session shell sharing one AutomationService (or connected daemon)
    Repl,
}

#[derive(Subcommand)]
enum DaemonCmd {
    /// Start the local 0600 Unix socket daemon
    Start {
        /// Run in foreground instead of detaching
        #[arg(long)]
        foreground: bool,
    },
    /// Check whether the daemon is running and reachable
    Status,
    /// Stop the running daemon
    Stop,
    /// Print or install a `systemd --user` unit file for persistent desktop sessions
    SystemdUnit {
        /// Write the unit file into ~/.config/systemd/user/automata-service.service
        #[arg(long)]
        install: bool,
        /// Also run `systemctl --user daemon-reload && systemctl --user enable --now automata-service`
        #[arg(long)]
        enable: bool,
    },
}

#[derive(Args, Clone, Default)]
struct TargetArgs {
    #[arg(long)]
    process: Option<String>,
    #[arg(long)]
    pid: Option<u32>,
    #[arg(long)]
    title: Option<String>,
    #[arg(long)]
    hwnd: Option<String>,
    #[arg(long)]
    selector: Option<String>,
}

impl TargetArgs {
    fn to_target(&self) -> Result<Target, ServiceError> {
        let window_id = self
            .hwnd
            .as_deref()
            .map(WindowId::parse_str)
            .transpose()
            .map_err(ServiceError::from)?;
        Ok(Target {
            process: self.process.clone(),
            pid: self.pid,
            title: self.title.clone(),
            automation_id: None,
            selector: self.selector.clone(),
            window_id,
        })
    }

    fn to_filter(&self) -> Result<WindowFilter, ServiceError> {
        let window_id = self
            .hwnd
            .as_deref()
            .map(WindowId::parse_str)
            .transpose()
            .map_err(ServiceError::from)?;
        Ok(WindowFilter {
            process: self.process.clone(),
            pid: self.pid,
            title: self.title.clone(),
            automation_id: None,
            window_id,
        })
    }
}

#[derive(Subcommand)]
enum ExploreCmd {
    Windows {
        #[command(flatten)]
        target: TargetArgs,
    },
    Focused,
    Tree {
        #[command(flatten)]
        target: TargetArgs,
        #[arg(long, default_value_t = 10)]
        max_results: usize,
    },
    Find {
        #[command(flatten)]
        target: TargetArgs,
        #[arg(long)]
        query: String,
        #[arg(long, default_value_t = 20)]
        max_results: usize,
        /// Query a previously saved SnapshotBundle directory (or tree.json) completely offline
        #[arg(long)]
        from_snapshot: Option<PathBuf>,
    },
    /// Capture an offline snapshot bundle (window metadata, full tree.json, PNG, optional OCR)
    CaptureBundle {
        #[command(flatten)]
        target: TargetArgs,
        /// Output directory for snapshot bundle files (defaults under ~/.ui-automata/logs/snapshots/)
        #[arg(long)]
        out: Option<PathBuf>,
        #[arg(long, default_value_t = 5_000)]
        max_nodes: usize,
        #[arg(long, default_value_t = 32)]
        max_depth: usize,
        /// Also capture OCR layout into ocr.json
        #[arg(long)]
        ocr: bool,
    },
    /// Execute a multi-step exploration probe script (continues on step errors by default)
    Batch {
        #[command(flatten)]
        target: TargetArgs,
        /// Path to JSON file containing an array of BatchProbeStep objects
        #[arg(long)]
        file: PathBuf,
        /// Directory to store per-step capture bundles when steps request tree/screenshot/ocr
        #[arg(long)]
        bundle_dir: Option<PathBuf>,
        /// Abort remaining probes on first failed step
        #[arg(long)]
        stop_on_error: bool,
    },
}

#[derive(Subcommand)]
enum ActCmd {
    Launch {
        target: String,
        #[arg(long, default_value = "new_pid")]
        wait: String,
        #[arg(long, default_value_t = 30_000)]
        timeout_ms: u64,
    },
    Focus {
        #[command(flatten)]
        target: TargetArgs,
    },
    Close {
        #[command(flatten)]
        target: TargetArgs,
    },
    Invoke {
        #[command(flatten)]
        target: TargetArgs,
        #[arg(long)]
        elem: String,
    },
    SetValue {
        #[command(flatten)]
        target: TargetArgs,
        #[arg(long)]
        elem: String,
        #[arg(long)]
        value: String,
    },
    Click {
        #[command(flatten)]
        target: TargetArgs,
        #[arg(long)]
        elem: String,
    },
    Steps {
        #[command(flatten)]
        target: TargetArgs,
        /// Path to JSON file containing an array of Step objects
        #[arg(long)]
        file: PathBuf,
    },
    MoveCursor {
        #[arg(long)]
        x: i32,
        #[arg(long)]
        y: i32,
    },
}

#[derive(Subcommand)]
enum VisionCmd {
    WindowLayout {
        #[arg(long)]
        hwnd: String,
    },
    ScreenLayout,
    Capture {
        #[arg(long)]
        hwnd: Option<String>,
        #[arg(long)]
        path: Option<String>,
    },
}

#[derive(Subcommand)]
enum WorkflowCmd {
    List,
    Lint {
        file: String,
    },
    Run {
        file: String,
    },
    Status,
    Cancel,
}

#[derive(Subcommand)]
enum RecordCmd {
    Start {
        name: String,
        #[command(flatten)]
        target: TargetArgs,
    },
    Status,
    Stop {
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum SystemCmd {
    Ping,
    Processes,
    Capabilities,
    Doctor {
        #[arg(long, default_value = "auto")]
        profile: String,
    },
}

fn build_config(workflows_dir: Option<PathBuf>, lifecycle: ConnectionLifecycle) -> ServiceConfig {
    let mut cfg = ServiceConfig {
        connection_mode: lifecycle,
        ..ServiceConfig::default()
    };
    if let Some(dir) = workflows_dir {
        cfg.workflows_dir = dir;
    } else if let Ok(env_dir) = std::env::var("AUTOMATA_WORKFLOWS_DIR") {
        cfg.workflows_dir = PathBuf::from(env_dir);
    } else if PathBuf::from("workflows").is_dir() {
        cfg.workflows_dir = PathBuf::from("workflows");
    }
    cfg
}

fn main() {
    let cli = Cli::parse();
    let sock = cli.socket.clone().unwrap_or_else(default_socket_path);

    match cli.cmd {
        TopCmd::Daemon { action } => match action {
            DaemonCmd::Start { foreground } => {
                if foreground {
                    let cfg = build_config(cli.workflows_dir, ConnectionLifecycle::Persistent);
                    let svc = AutomationService::open(cfg).expect("open AutomationService");
                    let shutdown = Arc::new(AtomicBool::new(false));
                    if let Err(err) = run_daemon_server(svc, &sock, shutdown) {
                        eprintln!("daemon error: {err}");
                        std::process::exit(1);
                    }
                } else {
                    if is_daemon_alive(&sock) {
                        println!("Daemon already running at {}", sock.display());
                        return;
                    }
                    let exe = std::env::current_exe().expect("current_exe");
                    let mut cmd = std::process::Command::new(exe);
                    cmd.args(["daemon", "start", "--foreground", "--socket"])
                        .arg(&sock);
                    if let Some(ref wf_dir) = cli.workflows_dir {
                        cmd.arg("--workflows-dir").arg(wf_dir);
                    }
                    match automata_linux::runtime::command::spawn_detached(cmd, "automata-daemon") {
                        Ok(pid) => {
                            for _ in 0..40 {
                                if is_daemon_alive(&sock) {
                                    println!("Started automata daemon (pid={pid}) at {}", sock.display());
                                    return;
                                }
                                std::thread::sleep(std::time::Duration::from_millis(50));
                            }
                            eprintln!("Daemon spawned (pid={pid}) but socket {} did not become ready", sock.display());
                            std::process::exit(1);
                        }
                        Err(e) => {
                            eprintln!("Failed to spawn daemon: {e}");
                            std::process::exit(1);
                        }
                    }
                }
            }
            DaemonCmd::Status => {
                if is_daemon_alive(&sock) {
                    match call_daemon(&sock, Command::Session(SessionCommand::Info)) {
                        Ok(res) => print_result(cli.json, &res),
                        Err(e) => {
                            eprintln!("Socket reachable but query failed: {e}");
                            std::process::exit(1);
                        }
                    }
                } else {
                    println!("Daemon not running ({})", sock.display());
                }
            }
            DaemonCmd::Stop => match stop_daemon(&sock) {
                Ok(true) => println!("Stopped automata daemon ({})", sock.display()),
                Ok(false) => println!("No running daemon found ({})", sock.display()),
                Err(e) => {
                    eprintln!("Failed to stop daemon: {e}");
                    std::process::exit(1);
                }
            },
            DaemonCmd::SystemdUnit { install, enable } => {
                let exe = std::env::current_exe().expect("current_exe");
                let unit = render_systemd_user_unit(&exe, &sock, cli.workflows_dir.as_deref());
                if !install && !enable {
                    print!("{unit}");
                    return;
                }
                let unit_path = default_systemd_unit_path();
                if let Some(parent) = unit_path.parent() {
                    if let Err(e) = std::fs::create_dir_all(parent) {
                        eprintln!("Failed to create {}: {e}", parent.display());
                        std::process::exit(1);
                    }
                }
                if let Err(e) = std::fs::write(&unit_path, &unit) {
                    eprintln!("Failed to write {}: {e}", unit_path.display());
                    std::process::exit(1);
                }
                println!("Wrote {}", unit_path.display());
                if enable {
                    let reload = std::process::Command::new("systemctl")
                        .args(["--user", "daemon-reload"])
                        .status();
                    let start = std::process::Command::new("systemctl")
                        .args(["--user", "enable", "--now", "automata-service.service"])
                        .status();
                    match (reload, start) {
                        (Ok(r), Ok(s)) if r.success() && s.success() => {
                            println!("Enabled and started automata-service.service");
                        }
                        (r, s) => {
                            eprintln!("systemctl --user failed: reload={r:?}, start={s:?}");
                            std::process::exit(1);
                        }
                    }
                }
            }
        },
        TopCmd::Repl => run_repl(&cli, &sock),
        other => {
            let cmd = match to_service_command(other) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("Error: {e}");
                    std::process::exit(2);
                }
            };
            let res = if !cli.no_daemon && is_daemon_alive(&sock) {
                call_daemon(&sock, cmd)
            } else {
                let cfg = build_config(cli.workflows_dir, ConnectionLifecycle::OneShot);
                AutomationService::open(cfg).and_then(|svc| svc.call(cmd))
            };
            match res {
                Ok(out) => print_result(cli.json, &out),
                Err(e) => {
                    eprintln!("Error: {e}");
                    std::process::exit(1);
                }
            }
        }
    }
}

fn to_service_command(top: TopCmd) -> Result<Command, ServiceError> {
    match top {
        TopCmd::Explore { action } => match action {
            ExploreCmd::Windows { target } => Ok(Command::Explore(ExploreCommand::ListWindows {
                filter: target.to_filter()?,
            })),
            ExploreCmd::Focused => Ok(Command::Explore(ExploreCommand::FocusedWindow)),
            ExploreCmd::Tree {
                target,
                max_results,
            } => Ok(Command::Explore(ExploreCommand::WindowTree {
                target: target.to_target()?,
                max_results,
            })),
            ExploreCmd::Find {
                target,
                query,
                max_results,
                from_snapshot,
            } => {
                if let Some(bundle_dir) = from_snapshot {
                    Ok(Command::Explore(ExploreCommand::QuerySnapshot {
                        bundle_dir,
                        selector: query,
                        max_results,
                    }))
                } else {
                    Ok(Command::Explore(ExploreCommand::FindElements {
                        target: target.to_target()?,
                        selector: query,
                        include_ancestors: true,
                        include_siblings: false,
                        max_results,
                    }))
                }
            }
            ExploreCmd::CaptureBundle {
                target,
                out,
                max_nodes,
                max_depth,
                ocr,
            } => Ok(Command::Explore(ExploreCommand::CaptureBundle {
                target: target.to_target()?,
                out_dir: out,
                max_nodes,
                max_depth,
                include_ocr: ocr,
            })),
            ExploreCmd::Batch {
                target,
                file,
                bundle_dir,
                stop_on_error,
            } => {
                let raw = std::fs::read_to_string(&file).map_err(|e| {
                    ServiceError::InvalidInput(format!("failed to read {}: {e}", file.display()))
                })?;
                let steps: Vec<BatchProbeStep> = serde_json::from_str(&raw).map_err(|e| {
                    ServiceError::InvalidInput(format!(
                        "invalid batch probe JSON in {}: {e}",
                        file.display()
                    ))
                })?;
                Ok(Command::Explore(ExploreCommand::Batch {
                    target: target.to_target()?,
                    steps,
                    stop_on_error,
                    bundle_dir,
                }))
            }
        },
        TopCmd::Act { action } => match action {
            ActCmd::Launch {
                target,
                wait,
                timeout_ms,
            } => {
                let wait_enum = match wait.as_str() {
                    "new_window" => LaunchWaitArg::NewWindow,
                    "match_any" => LaunchWaitArg::MatchAny,
                    _ => LaunchWaitArg::NewPid,
                };
                Ok(Command::Act(ActCommand::LaunchApp {
                    target,
                    wait: wait_enum,
                    timeout_ms,
                }))
            }
            ActCmd::Focus { target } => Ok(Command::Act(ActCommand::WindowState {
                target: target.to_target()?,
                state: WindowStateArg::Focus,
            })),
            ActCmd::Close { target } => Ok(Command::Act(ActCommand::WindowState {
                target: target.to_target()?,
                state: WindowStateArg::Close,
            })),
            ActCmd::Invoke { target, elem } => Ok(Command::Act(ActCommand::Invoke {
                window: target.to_target()?,
                selector: elem,
                timeout_secs: 10.0,
            })),
            ActCmd::SetValue {
                target,
                elem,
                value,
            } => Ok(Command::Act(ActCommand::SetValue {
                window: target.to_target()?,
                selector: elem,
                value,
                timeout_secs: 10.0,
            })),
            ActCmd::Click { target, elem } => Ok(Command::Act(ActCommand::ClickSelector {
                window: target.to_target()?,
                selector: elem,
                button: ClickButtonArg::Left,
                timeout_secs: 10.0,
            })),
            ActCmd::Steps { target, file } => {
                let content = std::fs::read_to_string(&file)?;
                let steps: Vec<serde_json::Value> = serde_json::from_str(&content)
                    .map_err(|e| ServiceError::InvalidInput(format!("invalid steps JSON: {e}")))?;
                Ok(Command::Act(ActCommand::RunSteps {
                    window: target.to_target()?,
                    steps,
                    reset: false,
                    timeout_secs: 10.0,
                }))
            }
            ActCmd::MoveCursor { x, y } => Ok(Command::Input(InputCommand::MouseMove {
                point: SpacePoint::screen(x, y),
            })),
        },
        TopCmd::Vision { action } => match action {
            VisionCmd::WindowLayout { hwnd } => Ok(Command::Vision(VisionCommand::WindowLayout {
                window_id: WindowId::parse_str(&hwnd)?,
                capture_only: false,
                save_screenshot: false,
            })),
            VisionCmd::ScreenLayout => Ok(Command::Vision(VisionCommand::ScreenLayout {
                capture_only: false,
                save_screenshot: false,
            })),
            VisionCmd::Capture { hwnd, path } => {
                let window_id = hwnd.as_deref().map(WindowId::parse_str).transpose()?;
                Ok(Command::Vision(VisionCommand::Capture {
                    window_id,
                    region: None,
                    path,
                }))
            }
        },
        TopCmd::Workflow { action } => match action {
            WorkflowCmd::List => Ok(Command::Workflow(WorkflowCommand::List)),
            WorkflowCmd::Lint { file } => Ok(Command::Workflow(WorkflowCommand::Lint {
                yaml: None,
                file: Some(file),
            })),
            WorkflowCmd::Run { file } => Ok(Command::Workflow(WorkflowCommand::RunFile {
                file,
                params: HashMap::new(),
                background: false,
            })),
            WorkflowCmd::Status => Ok(Command::Workflow(WorkflowCommand::Status)),
            WorkflowCmd::Cancel => Ok(Command::Workflow(WorkflowCommand::Cancel)),
        },
        TopCmd::Record { action } => match action {
            RecordCmd::Start { name, target } => {
                let tgt = target.to_target()?;
                Ok(Command::Session(SessionCommand::RecordStart {
                    name,
                    target: (!tgt.is_empty()).then_some(tgt),
                }))
            }
            RecordCmd::Status => Ok(Command::Session(SessionCommand::RecordStatus)),
            RecordCmd::Stop { out } => Ok(Command::Session(SessionCommand::RecordStop {
                output_path: out,
            })),
        },
        TopCmd::System { action } => match action {
            SystemCmd::Ping => Ok(Command::System(SystemCommand::Ping)),
            SystemCmd::Processes => Ok(Command::System(SystemCommand::ListProcesses)),
            SystemCmd::Capabilities => Ok(Command::Session(SessionCommand::Capabilities)),
            SystemCmd::Doctor { profile } => Ok(Command::System(SystemCommand::Doctor {
                profile: Some(profile),
            })),
        },
        TopCmd::Daemon { .. } | TopCmd::Repl => unreachable!(),
    }
}

fn run_repl(cli: &Cli, sock: &PathBuf) {
    let use_daemon = !cli.no_daemon && is_daemon_alive(sock);
    let local_svc = if use_daemon {
        None
    } else {
        let cfg = build_config(cli.workflows_dir.clone(), ConnectionLifecycle::Persistent);
        Some(AutomationService::open(cfg).expect("open REPL service"))
    };
    println!(
        "automata REPL ({}). Type 'help' or 'quit'.",
        if use_daemon { "daemon UDS" } else { "in-process session" }
    );
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line_res in stdin.lock().lines() {
        print!("automata> ");
        let _ = stdout.flush();
        let Ok(line) = line_res else { break };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if matches!(trimmed, "quit" | "exit") {
            break;
        }
        if trimmed == "help" {
            println!("Commands: ping | windows | focused | capabilities | record_status | <raw Command JSON>");
            continue;
        }
        let cmd: Result<Command, String> = match trimmed {
            "ping" => Ok(Command::System(SystemCommand::Ping)),
            "windows" => Ok(Command::Explore(ExploreCommand::ListWindows {
                filter: Default::default(),
            })),
            "focused" => Ok(Command::Explore(ExploreCommand::FocusedWindow)),
            "capabilities" => Ok(Command::Session(SessionCommand::Capabilities)),
            "record_status" => Ok(Command::Session(SessionCommand::RecordStatus)),
            json_cmd => serde_json::from_str::<Command>(json_cmd).map_err(|e| e.to_string()),
        };
        let cmd = match cmd {
            Ok(c) => c,
            Err(e) => {
                eprintln!("Parse error: {e}");
                continue;
            }
        };
        let res = match &local_svc {
            Some(svc) => svc.call(cmd),
            None => call_daemon(sock, cmd),
        };
        match res {
            Ok(out) => print_result(true, &out),
            Err(e) => eprintln!("Error: {e}"),
        }
    }
}

fn print_result(as_json: bool, res: &CommandResult) {
    if as_json {
        println!("{}", serde_json::to_string_pretty(&res.data).unwrap_or_default());
    } else {
        println!("{}", serde_json::to_string_pretty(&res.data).unwrap_or_default());
    }
}
