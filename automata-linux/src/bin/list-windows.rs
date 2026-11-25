/// List all top-level windows visible via AT-SPI2, or print platform diagnostics with `--diag`.
#[derive(clap::Parser)]
#[command(about = "List top-level AT-SPI2 windows and Linux backend diagnostics")]
struct Args {
    /// Print detected display/input/clipboard backends and environment overrides.
    #[arg(long)]
    diag: bool,
}

fn main() {
    use clap::Parser;
    let args = Args::parse();

    automata_linux::init_logging(None);
    automata_linux::init_a11y();

    if args.diag {
        let d = automata_linux::platform_diagnostics();
        println!("=== Platform Diagnostics ===");
        println!(
            "  display          : {} (override={})",
            d.display,
            d.display_override.as_deref().unwrap_or("auto")
        );
        println!(
            "  WAYLAND_DISPLAY  : {}",
            d.wayland_display.as_deref().unwrap_or("<unset>")
        );
        println!(
            "  DISPLAY (X11)    : {}",
            d.x11_display.as_deref().unwrap_or("<unset>")
        );
        println!(
            "  pointer_backend  : {} (override={})",
            d.pointer_backend,
            d.input_override.as_deref().unwrap_or("auto")
        );
        println!(
            "  keyboard_backend : {} (override={})",
            d.keyboard_backend,
            d.input_override.as_deref().unwrap_or("auto")
        );
        println!(
            "  clipboard        : {} (override={})",
            d.clipboard_backend,
            d.clipboard_override.as_deref().unwrap_or("auto")
        );
        println!(
            "  ocr_backend      : {} (override={})",
            d.ocr_backend,
            d.ocr_override.as_deref().unwrap_or("auto")
        );
        println!(
            "  screenshot       : {} (override={})",
            d.screenshot_backend,
            d.screenshot_override.as_deref().unwrap_or("auto")
        );
        println!(
            "  connection_mode  : {} (override={})",
            d.connection_mode,
            d.connection_override.as_deref().unwrap_or("auto")
        );
        println!(
            "  ort_dylib        : {}",
            d.ort_dylib.as_deref().unwrap_or("<not found>")
        );
        println!(
            "  ocr_model_dir    : {}",
            d.ocr_model_dir.as_deref().unwrap_or("<not found>")
        );
        println!("  ydotoold_running : {}", d.ydotoold_running);
        println!("  uinput_writable  : {}", d.uinput_writable);
        println!("  installed_tools  : {}", d.installed_tools.join(", "));
        return;
    }

    match automata_linux::application_windows() {
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
        Ok(windows) => {
            for (i, w) in windows.iter().enumerate() {
                println!("--- Window #{} ---", i + 1);
                println!("  hwnd    : {:#x}", w.hwnd);
                println!("  process : {} (pid {})", w.process_name, w.pid);
                println!("  title   : {}", w.title);
                println!("  type    : {}", w.control_type);
                println!("  id      : {}", w.automation_id);
                println!("  active  : {}", w.is_active);
                println!(
                    "  bounds  : x={} y={} w={} h={}",
                    w.x, w.y, w.width, w.height
                );
                println!("  dbus    : {} {}", w.bus_name, w.object_path);
            }
        }
    }
}
