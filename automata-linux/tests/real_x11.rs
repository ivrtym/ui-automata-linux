use std::thread;
use std::time::{Duration, Instant};

use automata_linux::Desktop;
use ui_automata::{AnchorDef, Desktop as _, Element as _, SelectorPath, ShadowDom};

fn find_win_by_title<'a>(
    wins: &'a [automata_linux::LinuxElement],
    pid: u32,
    title: &str,
) -> Option<&'a automata_linux::LinuxElement> {
    wins.iter().find(|w| {
        w.process_id().ok() == Some(pid) && w.name().as_deref() == Some(title)
    })
}

fn send_cmd(cmd_file: &str, cmd: &str) {
    std::fs::write(cmd_file, cmd).expect("failed to write cmd file");
}

#[test]
#[ignore = "requires live X11 + Openbox + AT-SPI2 + GTK3 harness (AUTOMATA_REAL_X11_PID)"]
fn real_x11_z_order_and_tooltip_windows() {
    let pid: u32 = std::env::var("AUTOMATA_REAL_X11_PID")
        .expect("AUTOMATA_REAL_X11_PID not set")
        .parse()
        .expect("invalid AUTOMATA_REAL_X11_PID");
    let cmd_file = std::env::var("AUTOMATA_REAL_X11_CMD_FILE")
        .expect("AUTOMATA_REAL_X11_CMD_FILE not set");

    let desktop = Desktop::new();

    // Wait for both GTK3 windows to register with AT-SPI2.
    let deadline = Instant::now() + Duration::from_secs(10);
    let (alpha_hwnd, beta_hwnd) = loop {
        let wins = desktop.application_windows().unwrap_or_default();
        if let (Some(a), Some(b)) = (
            find_win_by_title(&wins, pid, "X11 Test Alpha"),
            find_win_by_title(&wins, pid, "X11 Test Beta"),
        ) {
            println!(
                "Found windows: Alpha hwnd={:#x} bounds={:?}; Beta hwnd={:#x} bounds={:?}",
                a.hwnd().unwrap(),
                a.bounds(),
                b.hwnd().unwrap(),
                b.bounds()
            );
            break (a.hwnd().unwrap(), b.hwnd().unwrap());
        }
        assert!(Instant::now() < deadline, "timed out waiting for GTK3 windows in AT-SPI2");
        thread::sleep(Duration::from_millis(150));
    };

    // 1. Raise Alpha -> Alpha must precede Beta in hwnd_z_order() and ShadowDom pick Alpha.
    send_cmd(&cmd_file, "raise_a");
    let z1 = wait_z_order(&desktop, alpha_hwnd, beta_hwnd, Duration::from_secs(5));
    println!("After raise_a, hwnd_z_order={z1:#x?}");
    assert_precedes(&z1, alpha_hwnd, beta_hwnd, "raise_a");

    let mut dom_a = ShadowDom::new();
    let sel = SelectorPath::parse("[role=window]").unwrap();
    dom_a
        .mount(vec![AnchorDef::root("win", sel.clone()).with_pid(pid)], &desktop)
        .expect("ShadowDom mount after raise_a");
    let chosen_a = dom_a.get("win", &desktop).unwrap().name().unwrap();
    println!("ShadowDom picked after raise_a: {chosen_a:?}");
    assert_eq!(chosen_a, "X11 Test Alpha");

    // 2. Raise Beta -> Beta must precede Alpha in hwnd_z_order() and fresh ShadowDom pick Beta.
    send_cmd(&cmd_file, "raise_b");
    let z2 = wait_z_order(&desktop, beta_hwnd, alpha_hwnd, Duration::from_secs(5));
    println!("After raise_b, hwnd_z_order={z2:#x?}");
    assert_precedes(&z2, beta_hwnd, alpha_hwnd, "raise_b");

    let mut dom_b = ShadowDom::new();
    dom_b
        .mount(vec![AnchorDef::root("win", sel).with_pid(pid)], &desktop)
        .expect("ShadowDom mount after raise_b");
    let chosen_b_el = dom_b.get("win", &desktop).unwrap().clone();
    let chosen_b = chosen_b_el.name().unwrap();
    println!("ShadowDom picked after raise_b: {chosen_b:?}");
    assert_eq!(chosen_b, "X11 Test Beta");

    // 3. Hover over "Button Beta" to trigger GTK3 tooltip and probe tooltip_windows().
    let btn_sel = SelectorPath::parse(">> [role=button][name='Button Beta']").unwrap();
    let btn = dom_b
        .find_descendant("win", &btn_sel, &desktop)
        .unwrap()
        .expect("Button Beta not found under X11 Test Beta");
    let (bx, by, bw, bh) = btn.bounds().expect("Button Beta bounds");
    let (cx, cy) = (bx + bw / 2, by + bh / 2);
    println!("Hovering Button Beta at ({cx}, {cy}) bounds=({bx},{by},{bw},{bh})");
    desktop.pointer_move(10, 10).unwrap();
    thread::sleep(Duration::from_millis(120));
    desktop.pointer_move(cx, cy).unwrap();

    let tt_deadline = Instant::now() + Duration::from_secs(5);
    let mut found_tooltips = Vec::new();
    while Instant::now() < tt_deadline {
        let tts = desktop.tooltip_windows();
        if !tts.is_empty() {
            found_tooltips = tts;
            break;
        }
        thread::sleep(Duration::from_millis(150));
    }

    println!("tooltip_windows() count={}", found_tooltips.len());
    for t in &found_tooltips {
        println!(
            "  tooltip: role={:?} name={:?} text={:?} inner_text={:?} visible={:?}",
            t.role(),
            t.name(),
            t.text(),
            t.inner_text(),
            t.is_visible()
        );
    }

    // Diagnostic: if empty, inspect AT-SPI2 children under app and under window to see where GTK3 placed the tooltip.
    if found_tooltips.is_empty() {
        if let Some(app) = chosen_b_el.parent() {
            println!("Diagnostic: App children when hovering:");
            for c in app.children().unwrap_or_default() {
                println!(
                    "  app child: role={:?} raw_role={:?} name={:?} visible={:?}",
                    c.role(),
                    c.raw_role_name(),
                    c.name(),
                    c.is_visible()
                );
                for gc in c.children().unwrap_or_default() {
                    println!(
                        "    grandchild: role={:?} raw_role={:?} name={:?} text={:?} visible={:?}",
                        gc.role(),
                        gc.raw_role_name(),
                        gc.name(),
                        gc.text(),
                        gc.is_visible()
                    );
                }
            }
        }
    }

    assert!(
        !found_tooltips.is_empty(),
        "expected desktop.tooltip_windows() to return active tooltip"
    );
    assert!(
        found_tooltips
            .iter()
            .any(|t| t.role() == "tool tip" && t.name().as_deref() == Some("Beta Hover Tooltip")),
        "expected tooltip with role='tool tip' and name='Beta Hover Tooltip'"
    );
    // Ensure tooltip window did not pollute application_windows().
    let app_wins = desktop.application_windows().unwrap_or_default();
    assert!(
        !app_wins
            .iter()
            .any(|w| w.name().as_deref() == Some("Beta Hover Tooltip")),
        "tooltip must not appear in application_windows()"
    );
}

fn wait_z_order(
    desktop: &Desktop,
    first: u64,
    second: u64,
    timeout: Duration,
) -> Vec<u64> {
    let deadline = Instant::now() + timeout;
    loop {
        let z = desktop.hwnd_z_order();
        let p1 = z.iter().position(|&h| h == first);
        let p2 = z.iter().position(|&h| h == second);
        if matches!((p1, p2), (Some(i1), Some(i2)) if i1 < i2) || Instant::now() >= deadline {
            return z;
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn assert_precedes(z: &[u64], first: u64, second: u64, phase: &str) {
    let p1 = z.iter().position(|&h| h == first);
    let p2 = z.iter().position(|&h| h == second);
    assert!(
        matches!((p1, p2), (Some(i1), Some(i2)) if i1 < i2),
        "{phase}: expected {first:#x} before {second:#x} in z_order={z:#x?}"
    );
}
