/// UI Workflow runner binary for Linux (AT-SPI2 + CDP).
///
/// Usage (interactive / TTY):
///   ui-workflow <script.yml> [--project-dir <dir>] [-- --param1 value1 ...]
///
/// Usage (pipe mode — spawned by automata-client):
///   ui-workflow <script.yml> --log-path <path.log> [-- --param1 value1 ...]
static CANCEL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn phase_event_to_json(evt: &ui_automata::PhaseEvent) -> String {
    use ui_automata::PhaseEvent;
    let v = match evt {
        PhaseEvent::PhaseStarted(phase) => {
            serde_json::json!({"type": "PhaseStarted", "phase": phase})
        }
        PhaseEvent::PhaseCompleted(phase) => {
            serde_json::json!({"type": "PhaseCompleted", "phase": phase})
        }
        PhaseEvent::PhaseSkipped(phase) => {
            serde_json::json!({"type": "PhaseSkipped", "phase": phase})
        }
        PhaseEvent::PhaseFailed { phase, error } => {
            serde_json::json!({"type": "PhaseFailed", "phase": phase, "error": error})
        }
        PhaseEvent::Completed | PhaseEvent::Failed(_) => return String::new(),
    };
    v.to_string()
}

fn tail_lines(path: &std::path::Path, n: usize) -> String {
    let content = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => return String::new(),
    };
    let lines: Vec<&str> = content.lines().collect();
    let skip = lines.len().saturating_sub(n);
    lines[skip..].join("\n")
}

fn main() {
    use std::collections::HashMap;
    use std::io::IsTerminal as _;
    use std::path::PathBuf;

    use clap::Parser;

    #[derive(Parser)]
    #[command(name = "ui-workflow")]
    struct Cli {
        /// Path to the workflow YAML script.
        script: PathBuf,

        /// Root directory for run logs (interactive mode only).
        /// Defaults to ~/.ui-automata/logs/
        #[arg(long)]
        project_dir: Option<PathBuf>,

        /// Explicit log file path (pipe mode).
        #[arg(long)]
        log_path: Option<PathBuf>,

        /// Screenshot capture channel (`auto`, `x11rb`, `screencast`, `grim`, `gnome-screenshot`, `import`, `scrot`, `portal`).
        #[arg(long, value_name = "CHANNEL")]
        screenshot_backend: Option<String>,

        /// Connection lifecycle mode (`auto`, `oneshot`, `persistent`).
        #[arg(long, value_name = "MODE")]
        connection_mode: Option<String>,

        /// Script parameter overrides (everything after `--`).
        /// Format: --key value (kebab-case mapped to snake_case)
        #[arg(last = true)]
        script_args: Vec<String>,
    }

    automata_linux::init_a11y();
    spawn_signal_listener();
    let cli = Cli::parse();
    if let Err(e) = automata_linux::apply_screenshot_backend_arg(cli.screenshot_backend.as_deref()) {
        eprintln!("invalid --screenshot-backend: {e}");
        std::process::exit(2);
    }
    if let Err(e) = automata_linux::apply_connection_mode_arg(cli.connection_mode.as_deref()) {
        eprintln!("invalid --connection-mode: {e}");
        std::process::exit(2);
    }

    let mut params: HashMap<String, String> = HashMap::new();
    let mut iter = cli.script_args.iter();
    while let Some(flag) = iter.next() {
        if let Some(key) = flag.strip_prefix("--") {
            let snake = key.replace('-', "_");
            let value = iter.next().cloned().unwrap_or_default();
            params.insert(snake, value);
        }
    }

    let pipe_mode = !std::io::stdout().is_terminal();

    let log_path: PathBuf = if let Some(p) = cli.log_path {
        p
    } else {
        let yaml = match std::fs::read_to_string(&cli.script) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("cannot read {}: {e}", cli.script.display());
                std::process::exit(1);
            }
        };

        let project_dir = match cli.project_dir {
            Some(d) => d,
            None => {
                let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
                PathBuf::from(home).join(".ui-automata").join("logs")
            }
        };

        let script_stem = cli
            .script
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let folder_name =
            ui_automata::yaml::WorkflowName::read(&yaml).unwrap_or_else(|| script_stem.clone());

        let run_dir = project_dir.join(&folder_name);
        if let Err(e) = std::fs::create_dir_all(&run_dir) {
            log::warn!("could not create run dir {}: {e}", run_dir.display());
        }

        let timestamp = {
            use time::macros::format_description;
            let fmt = format_description!("[year][month][day]T[hour][minute][second]");
            time::OffsetDateTime::now_local()
                .unwrap_or_else(|_| time::OffsetDateTime::now_utc())
                .format(fmt)
                .unwrap_or_else(|_| "00000000T000000".into())
        };

        run_dir.join(format!("{timestamp}.log"))
    };

    if pipe_mode {
        automata_linux::init_logging_file_only(&log_path);
    } else {
        automata_linux::init_logging(Some(&log_path));
        log::info!("log → {}", log_path.display());
    }
    let diag = automata_linux::platform_diagnostics();
    log::debug!(
        "backends: display={} pointer={} keyboard={} clipboard={} ydotoold={}",
        diag.display,
        diag.pointer_backend,
        diag.keyboard_backend,
        diag.clipboard_backend,
        diag.ydotoold_running
    );

    let script_str = cli.script.to_string_lossy().into_owned();
    let workflow = match ui_automata::yaml::WorkflowFile::load(&script_str, &params) {
        Ok(wf) => wf,
        Err(e) => {
            if pipe_mode {
                let json = serde_json::json!({"type": "Failed", "error": e.to_string()});
                println!("{json}");
            } else {
                log::error!("failed to load workflow: {e}");
            }
            std::process::exit(1);
        }
    };

    let desktop = automata_linux::Desktop::new();
    // In `auto` mode, reuse X11 connections/atoms for the workflow run without forcing persistent ScreenCast.
    // Explicit `--connection-mode oneshot` stays purely per-call; `persistent` is already held by `desktop`.
    let _workflow_capture_scope = if !automata_linux::is_connection_mode_explicit() {
        Some(desktop.x11_capture_session())
    } else {
        None
    };
    let mut executor = ui_automata::Executor::new(desktop);

    if pipe_mode {
        use std::io::Write as _;

        std::thread::spawn(|| {
            use std::io::Read as _;
            let mut buf = [0u8; 1];
            loop {
                match std::io::stdin().read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
            CANCEL.store(true, std::sync::atomic::Ordering::Relaxed);
        });

        let cancel_flag = Some(&CANCEL as &std::sync::atomic::AtomicBool);

        let result = workflow.run(
            &mut executor,
            Some(&mut |evt| {
                let line = phase_event_to_json(&evt);
                if !line.is_empty() {
                    println!("{line}");
                    let _ = std::io::stdout().flush();
                }
            }),
            cancel_flag,
        );

        drop(executor);

        let terminal_json = match result {
            Ok(state) => {
                log::info!("outputs: {}", outputs_to_json(state.output));
                serde_json::json!({"type": "Completed"}).to_string()
            }
            Err(e) => {
                let tail = tail_lines(&log_path, 100);
                let error = if tail.is_empty() {
                    e.to_string()
                } else {
                    format!(
                        "{e}\n\n--- last {n} lines of trace ---\n{tail}",
                        n = tail.lines().count()
                    )
                };
                serde_json::json!({"type": "Failed", "error": error}).to_string()
            }
        };
        println!("{terminal_json}");
        let _ = std::io::stdout().flush();
    } else {
        let cancel_flag = Some(&CANCEL as &std::sync::atomic::AtomicBool);
        match workflow.run(&mut executor, None, cancel_flag) {
            Ok(state) => {
                log::info!("outputs: {}", outputs_to_json(state.output));
                log::info!("completed successfully");
            }
            Err(e) => {
                log::error!("workflow failed: {e}");
                std::process::exit(1);
            }
        }
    }
}

fn spawn_signal_listener() {
    std::thread::spawn(|| {
        let Ok(rt) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return;
        };
        rt.block_on(async {
            use tokio::signal::unix::{SignalKind, signal};
            let mut sigterm = signal(SignalKind::terminate()).ok();
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    CANCEL.store(true, std::sync::atomic::Ordering::Relaxed);
                }
                _ = async {
                    if let Some(ref mut s) = sigterm {
                        let _ = s.recv().await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                } => {
                    CANCEL.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            }
        });
    });
}

fn outputs_to_json(output: ui_automata::Output) -> serde_json::Value {
    let map: serde_json::Map<String, serde_json::Value> = output
        .into_map()
        .into_iter()
        .map(|(k, v)| {
            let val = if v.len() == 1 {
                serde_json::Value::String(v.into_iter().next().unwrap())
            } else {
                serde_json::Value::Array(v.into_iter().map(serde_json::Value::String).collect())
            };
            (k, val)
        })
        .collect();
    serde_json::Value::Object(map)
}
