use std::io::{self, Write};

use automata_linux::doctor::{
    CheckStatus, DoctorReport, EnvProfile, RemediationAction, apply_remediation,
    build_privileged_script, inspect_environment,
};
use clap::Parser;

/// Linux desktop automation environment detector & auto-setup tool (`ui-doctor`).
#[derive(Parser)]
#[command(about = "Inspect Linux UI-Automata dependencies/permissions and auto-install/authorize")]
struct Args {
    /// Target environment profile: auto, x11, wayland, xvfb, ocr, all.
    #[arg(long, default_value = "auto")]
    profile: String,

    /// Emit full diagnostic report and remediation plan as JSON.
    #[arg(long)]
    json: bool,

    /// Print the generated privileged remediation script without executing it.
    #[arg(long)]
    dry_run: bool,

    /// Execute package installation and /dev/uinput udev authorization (uses sudo or pkexec).
    #[arg(long)]
    fix: bool,

    /// Skip confirmation prompt when used with `--fix`.
    #[arg(short = 'y', long)]
    yes: bool,
}

fn status_badge(s: CheckStatus) -> &'static str {
    match s {
        CheckStatus::Ok => "[OK]   ",
        CheckStatus::Missing => "[MISS] ",
        CheckStatus::PermissionDenied => "[PERM] ",
        CheckStatus::Degraded => "[WARN] ",
    }
}

fn print_report(report: &DoctorReport) {
    println!("=== UI-Automata Linux Environment Doctor ===");
    println!("  distro          : {}", report.distro);
    println!("  package_manager : {}", report.package_manager.as_str());
    println!(
        "  display         : {} (wayland={:?}, x11={:?})",
        report.diagnostics.display,
        report.diagnostics.wayland_display.as_deref().unwrap_or(""),
        report.diagnostics.x11_display.as_deref().unwrap_or("")
    );
    println!(
        "  active_backends : pointer={}, keyboard={}, clipboard={}, ocr={}, screenshot={}",
        report.diagnostics.pointer_backend,
        report.diagnostics.keyboard_backend,
        report.diagnostics.clipboard_backend,
        report.diagnostics.ocr_backend,
        report.diagnostics.screenshot_backend
    );
    println!(
        "  user / uinput   : user={}, in_input_group={}, uinput_exists={}, uinput_writable={}, udev_rule={}",
        report.current_user,
        report.in_input_group,
        report.uinput_exists,
        report.diagnostics.uinput_writable,
        report.udev_rule_present
    );

    println!("\n=== Checks ({:?}) ===", report.profile);
    for c in &report.checks {
        println!("  {} {:<16} {} — {}", status_badge(c.status), c.id, c.title, c.detail);
    }

    if report.actions.is_empty() {
        println!("\n环境已全部就绪，无需额外安装或授权。");
        return;
    }

    println!("\n=== Remediation Plan ===");
    for (idx, act) in report.actions.iter().enumerate() {
        match act {
            RemediationAction::InstallPackages {
                packages, command, ..
            } => {
                println!("  {}. 安装缺失软件包 ({}):", idx + 1, packages.join(", "));
                println!("     {command}");
            }
            RemediationAction::ConfigureUinput { user, commands, .. } => {
                println!("  {}. 授权用户 `{user}` 访问 /dev/uinput:", idx + 1);
                for c in commands {
                    println!("     {c}");
                }
            }
            RemediationAction::StartService { name, command } => {
                println!("  {}. 启动守护进程 `{name}`:", idx + 1);
                println!("     {command}");
            }
            RemediationAction::ManualHint { title, detail } => {
                println!("  {}. [提示] {title}: {detail}", idx + 1);
            }
        }
    }
}

fn confirm_prompt(script: &str) -> bool {
    println!("\n将执行以下提权脚本 (sudo / pkexec):\n---\n{script}\n---");
    print!("确认继续安装并配置权限? [y/N]: ");
    let _ = io::stdout().flush();
    let mut line = String::new();
    if io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

fn main() {
    let args = Args::parse();
    let profile = match EnvProfile::parse(&args.profile) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(2);
        }
    };

    let report = inspect_environment(profile);

    if args.json && !args.fix {
        let out = if args.dry_run {
            let outcome = apply_remediation(&report, true).ok();
            serde_json::json!({ "report": report, "dry_run": outcome })
        } else {
            serde_json::json!({ "report": report })
        };
        println!("{}", serde_json::to_string_pretty(&out).unwrap());
        if !report.is_ready() {
            std::process::exit(1);
        }
        return;
    }

    print_report(&report);

    if args.dry_run && !args.fix {
        if let Some(script) = build_privileged_script(&report.actions) {
            println!("\n=== Dry-Run Privileged Script ===\n{script}");
        }
        return;
    }

    if args.fix {
        if let Some(script) = build_privileged_script(&report.actions) {
            if !args.yes && !confirm_prompt(&script) {
                println!("已取消执行。");
                std::process::exit(1);
            }
        }
        match apply_remediation(&report, args.dry_run) {
            Ok(outcome) => {
                if args.json {
                    let post = inspect_environment(profile);
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "outcome": outcome,
                            "post_check": post,
                        }))
                        .unwrap()
                    );
                } else {
                    println!("\n=== Apply Outcome ===");
                    println!("  executed         : {}", outcome.executed);
                    println!(
                        "  elevation_method : {}",
                        outcome.elevation_method.as_deref().unwrap_or("none")
                    );
                    println!("  started_ydotoold : {}", outcome.started_ydotoold);
                    for note in outcome.notes {
                        println!("  - {note}");
                    }
                    let post = inspect_environment(profile);
                    println!("\n=== Post-Check Summary ===");
                    for c in &post.checks {
                        println!("  {} {:<16} {}", status_badge(c.status), c.id, c.detail);
                    }
                }
            }
            Err(e) => {
                eprintln!("Auto-fix failed: {e}");
                std::process::exit(1);
            }
        }
    }
}
