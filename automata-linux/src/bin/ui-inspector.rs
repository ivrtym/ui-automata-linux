/// Live AT-SPI2 UI Inspector for Linux.
///
/// Polls the cursor position (via `xdotool getmouselocation` when available)
/// or inspects a specific `--x <X> --y <Y>` screen coordinate against the active
/// window's AT-SPI2 tree.
use std::time::Duration;

use ui_automata::Element as _;

#[derive(clap::Parser)]
#[command(about = "Inspect the AT-SPI2 element under the cursor or at (x, y)")]
struct Args {
    /// Inspect a single coordinate once and exit.
    #[arg(long)]
    x: Option<i32>,
    #[arg(long)]
    y: Option<i32>,
}

fn hit_test(el: &automata_linux::LinuxElement, x: i32, y: i32, path: &mut Vec<automata_linux::LinuxElement>) {
    path.push(el.clone());
    if let Ok(children) = el.children() {
        for child in children.into_iter().rev() {
            if let Ok((bx, by, bw, bh)) = child.bounds() {
                if bw > 0 && bh > 0 && x >= bx && x <= bx + bw && y >= by && y <= by + bh {
                    hit_test(&child, x, y, path);
                    return;
                }
            }
        }
    }
}

fn print_chain(x: i32, y: i32, chain: &[automata_linux::LinuxElement]) {
    println!("Cursor ({x}, {y}):");
    for (i, node) in chain.iter().enumerate() {
        let indent = "  ".repeat(i);
        let prefix = if i == 0 { "" } else { "└─ " };
        let role = node.role();
        let name = node.name().unwrap_or_default();
        let id = node.automation_id().unwrap_or_default();
        let bounds = node.bounds().unwrap_or((0, 0, 0, 0));
        println!(
            "{indent}{prefix}[{role}] {:?} id={:?} rect=({},{},{},{})",
            name, id, bounds.0, bounds.1, bounds.2, bounds.3
        );
    }
}

fn inspect_point(desktop: &automata_linux::Desktop, x: i32, y: i32) {
    let Some(win) = desktop.foreground_window().or_else(|| {
        desktop.application_windows().ok()?.into_iter().find(|w| {
            w.bounds()
                .map(|(bx, by, bw, bh)| x >= bx && x <= bx + bw && y >= by && y <= by + bh)
                .unwrap_or(false)
        })
    }) else {
        println!("No matching window at ({x}, {y})");
        return;
    };

    let mut chain = Vec::new();
    hit_test(&win, x, y, &mut chain);
    print_chain(x, y, &chain);
}

fn main() {
    use clap::Parser;
    let args = Args::parse();

    automata_linux::init_logging(None);
    automata_linux::init_a11y();
    let desktop = automata_linux::Desktop::new();

    if let (Some(x), Some(y)) = (args.x, args.y) {
        inspect_point(&desktop, x, y);
        return;
    }

    println!("Live AT-SPI2 Inspector running (Ctrl-C to exit)...");
    let mut last_pos = None;
    loop {
        match automata_linux::input::get_cursor_pos() {
            Ok((x, y)) => {
                if last_pos != Some((x, y)) {
                    last_pos = Some((x, y));
                    print!("\x1b[2J\x1b[H");
                    inspect_point(&desktop, x, y);
                }
            }
            Err(e) => {
                eprintln!("{e}");
                eprintln!("Tip: pass --x <X> --y <Y> to inspect a point directly.");
                std::process::exit(1);
            }
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}
