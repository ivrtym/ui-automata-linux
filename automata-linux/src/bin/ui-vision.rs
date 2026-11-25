/// Linux OCR Vision Fallback CLI (`ui-vision`).
///
/// Subcommands:
///   ui-vision screen [--json]
///   ui-vision window (--hwnd <0x...> | --title <str>) [--json]
///   ui-vision image <path> [--json]
///   ui-vision click-text <query> [--hwnd <0x...>] [--title <str>]
use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "ui-vision", about = "Linux OCR vision fallback for UI Automata")]
struct Cli {
    /// Screenshot capture channel (`auto`, `x11rb`, `screencast`, `grim`, `gnome-screenshot`, `import`, `scrot`, `portal`).
    #[arg(long, global = true, value_name = "CHANNEL")]
    screenshot_backend: Option<String>,

    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Capture full screen and run OCR.
    Screen {
        #[arg(long)]
        json: bool,
    },
    /// Capture a window and run OCR.
    Window {
        #[arg(long)]
        hwnd: Option<String>,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Run OCR on a local image file.
    Image {
        path: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Find text via OCR and click its center.
    ClickText {
        query: String,
        #[arg(long)]
        hwnd: Option<String>,
        #[arg(long)]
        title: Option<String>,
    },
}

fn resolve_hwnd(hwnd: Option<&str>, title: Option<&str>) -> Option<u64> {
    if let Some(h) = hwnd {
        let hex = h.strip_prefix("0x").or_else(|| h.strip_prefix("0X")).unwrap_or(h);
        return u64::from_str_radix(hex, 16)
            .or_else(|_| h.parse::<u64>())
            .ok();
    }
    if let Some(t) = title {
        let t_low = t.to_lowercase();
        let wins = automata_linux::application_windows().ok()?;
        return wins
            .into_iter()
            .find(|w| w.title.to_lowercase().contains(&t_low))
            .map(|w| w.hwnd);
    }
    None
}

fn print_layout(layout: &automata_linux::OcrLayout, as_json: bool) {
    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(layout).expect("json serialize failed")
        );
        return;
    }
    println!(
        "OCR backend={} size={}x{} origin=({},{}) blocks={}",
        layout.backend,
        layout.width,
        layout.height,
        layout.origin_x,
        layout.origin_y,
        layout.blocks.len()
    );
    for (i, b) in layout.blocks.iter().enumerate() {
        let (cx, cy) = b.center();
        println!(
            "[{i}] conf={:.2} rect=({},{},{},{}) center=({cx},{cy}) text={:?}",
            b.confidence, b.x, b.y, b.width, b.height, b.text
        );
    }
}

fn main() {
    automata_linux::init_logging(None);
    let cli = Cli::parse();
    if let Err(e) = automata_linux::apply_screenshot_backend_arg(cli.screenshot_backend.as_deref()) {
        eprintln!("Error: {e}");
        std::process::exit(2);
    }

    match cli.command {
        Cmd::Screen { json } => match automata_linux::screen_layout() {
            Ok(l) => print_layout(&l, json),
            Err(e) => {
                eprintln!("Error: {e}");
                std::process::exit(1);
            }
        },
        Cmd::Window { hwnd, title, json } => {
            let Some(h) = resolve_hwnd(hwnd.as_deref(), title.as_deref()) else {
                eprintln!("Error: matching window not found (supply --hwnd or --title)");
                std::process::exit(1);
            };
            match automata_linux::window_layout(h) {
                Ok(l) => print_layout(&l, json),
                Err(e) => {
                    eprintln!("Error: {e}");
                    std::process::exit(1);
                }
            }
        }
        Cmd::Image { path, json } => match automata_linux::recognize_image(&path) {
            Ok(l) => print_layout(&l, json),
            Err(e) => {
                eprintln!("Error: {e}");
                std::process::exit(1);
            }
        },
        Cmd::ClickText { query, hwnd, title } => {
            let target_hwnd = if hwnd.is_some() || title.is_some() {
                let Some(h) = resolve_hwnd(hwnd.as_deref(), title.as_deref()) else {
                    eprintln!("Error: matching window not found");
                    std::process::exit(1);
                };
                Some(h)
            } else {
                None
            };
            match automata_linux::click_text(target_hwnd, &query, automata_linux::input::ClickType::Left) {
                Ok(b) => {
                    let (cx, cy) = b.center();
                    println!("Clicked {:?} at ({cx}, {cy})", b.text);
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    std::process::exit(1);
                }
            }
        }
    }
}
