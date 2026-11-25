//! Pure-Rust X11 input backend via `x11rb::protocol::xtest` + core X11
//! keymap resolution with RAII key/button release protection.

use std::time::Duration;

use x11rb::connection::Connection;
use x11rb::protocol::xproto::ConnectionExt;
use x11rb::protocol::xtest::ConnectionExt as XTestExt;

use crate::input::guard::InputReleaseGuard;
use crate::input::{ClickType, ScrollAxis};
use crate::{Result, UiError};

const X11_KEY_PRESS: u8 = 2;
const X11_KEY_RELEASE: u8 = 3;
const X11_BUTTON_PRESS: u8 = 4;
const X11_BUTTON_RELEASE: u8 = 5;
const X11_MOTION_NOTIFY: u8 = 6;

fn with_xtest<F, T>(op: F) -> Result<T>
where
    F: FnOnce(&x11rb::rust_connection::RustConnection, x11rb::protocol::xproto::Window) -> Result<T>,
{
    let (conn, screen_num) = x11rb::connect(None)
        .map_err(|e| UiError::Platform(format!("x11rb connect failed: {e}")))?;
    let ext = conn
        .xtest_get_version(2, 1)
        .map_err(|e| UiError::Platform(format!("XTEST query failed: {e}")))?
        .reply()
        .map_err(|e| UiError::Platform(format!("XTEST extension unavailable on display: {e}")))?;
    let _ = ext;
    let root = conn.setup().roots[screen_num].root;
    op(&conn, root)
}

fn fake_motion(
    conn: &x11rb::rust_connection::RustConnection,
    root: x11rb::protocol::xproto::Window,
    x: i32,
    y: i32,
) -> Result<()> {
    conn.xtest_fake_input(
        X11_MOTION_NOTIFY,
        0,
        0,
        root,
        x.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
        y.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
        0,
    )
    .map_err(|e| UiError::Platform(format!("xtest motion failed: {e}")))?;
    conn.flush()
        .map_err(|e| UiError::Platform(format!("x11 flush failed: {e}")))?;
    Ok(())
}

fn fake_button(conn: &x11rb::rust_connection::RustConnection, press: bool, button: u8) {
    let ty = if press {
        X11_BUTTON_PRESS
    } else {
        X11_BUTTON_RELEASE
    };
    let _ = conn.xtest_fake_input(ty, button, 0, x11rb::NONE, 0, 0, 0);
    let _ = conn.flush();
}

fn fake_key(conn: &x11rb::rust_connection::RustConnection, press: bool, keycode: u8) {
    let ty = if press {
        X11_KEY_PRESS
    } else {
        X11_KEY_RELEASE
    };
    let _ = conn.xtest_fake_input(ty, keycode, 0, x11rb::NONE, 0, 0, 0);
    let _ = conn.flush();
}

pub fn query_pointer() -> Result<(i32, i32)> {
    let (conn, screen_num) = x11rb::connect(None)
        .map_err(|e| UiError::Platform(format!("x11rb connect failed: {e}")))?;
    let root = conn.setup().roots[screen_num].root;
    let reply = conn
        .query_pointer(root)
        .map_err(|e| UiError::Platform(format!("query_pointer failed: {e}")))?
        .reply()
        .map_err(|e| UiError::Platform(format!("query_pointer reply failed: {e}")))?;
    Ok((i32::from(reply.root_x), i32::from(reply.root_y)))
}

pub fn mouse_move(x: i32, y: i32) -> Result<()> {
    with_xtest(|conn, root| fake_motion(conn, root, x, y))
}

pub fn mouse_click(x: i32, y: i32, kind: ClickType) -> Result<()> {
    with_xtest(|conn, root| {
        fake_motion(conn, root, x, y)?;
        let (button, repeats) = match kind {
            ClickType::Left => (1u8, 1usize),
            ClickType::Double => (1u8, 2usize),
            ClickType::Triple => (1u8, 3usize),
            ClickType::Middle => (2u8, 1usize),
            ClickType::Right => (3u8, 1usize),
        };
        for i in 0..repeats {
            let mut guard = InputReleaseGuard::new(|btn| fake_button(conn, false, btn));
            fake_button(conn, true, button);
            guard.push(button);
            std::thread::sleep(Duration::from_millis(10));
            guard.release_all();
            if i + 1 < repeats {
                std::thread::sleep(Duration::from_millis(25));
            }
        }
        Ok(())
    })
}

pub fn mouse_drag(x1: i32, y1: i32, x2: i32, y2: i32) -> Result<()> {
    with_xtest(|conn, root| {
        fake_motion(conn, root, x1, y1)?;
        let mut guard = InputReleaseGuard::new(|btn| fake_button(conn, false, btn));
        fake_button(conn, true, 1);
        guard.push(1);
        std::thread::sleep(Duration::from_millis(30));
        fake_motion(conn, root, x2, y2)?;
        std::thread::sleep(Duration::from_millis(30));
        guard.release_all();
        Ok(())
    })
}

pub fn scroll_axis_ticks(delta: i32, axis: ScrollAxis) -> Result<()> {
    if delta == 0 {
        return Ok(());
    }
    let button = match (axis, delta > 0) {
        (ScrollAxis::Vertical, true) => 4u8,
        (ScrollAxis::Vertical, false) => 5u8,
        (ScrollAxis::Horizontal, true) => 7u8,
        (ScrollAxis::Horizontal, false) => 6u8,
    };
    let ticks = delta.unsigned_abs().max(1);
    with_xtest(|conn, _root| {
        for i in 0..ticks {
            let mut guard = InputReleaseGuard::new(|btn| fake_button(conn, false, btn));
            fake_button(conn, true, button);
            guard.push(button);
            guard.release_all();
            if i + 1 < ticks {
                std::thread::sleep(Duration::from_millis(12));
            }
        }
        Ok(())
    })
}

/// Sends a normalised X11 keysym chord (e.g. `"ctrl+shift+Return"`, `"F4"`)
/// via XTEST, automatically pressing Shift if an uppercase ASCII character or
/// shifted symbol lives in column 1 of the server's keyboard map.
pub fn key_press_chord(normalised: &str) -> Result<()> {
    with_xtest(|conn, _root| {
        let setup = conn.setup();
        let min_kc = setup.min_keycode;
        let max_kc = setup.max_keycode;
        let count = max_kc.saturating_sub(min_kc).saturating_add(1);
        let map = conn
            .get_keyboard_mapping(min_kc, count)
            .map_err(|e| UiError::Platform(format!("get_keyboard_mapping failed: {e}")))?
            .reply()
            .map_err(|e| UiError::Platform(format!("get_keyboard_mapping reply failed: {e}")))?;

        let syms_per_kc = usize::from(map.keysyms_per_keycode.max(1));
        let mut sequence: Vec<(u8, bool)> = Vec::new();
        for token in normalised.split('+').map(str::trim).filter(|s| !s.is_empty()) {
            let keysym = token_to_x11_keysym(token)?;
            let (kc, needs_shift) = lookup_keycode_for_keysym(&map.keysyms, syms_per_kc, min_kc, keysym)
                .ok_or_else(|| {
                    UiError::Platform(format!(
                        "X11 keymap lacks keysym {keysym:#x} for token {token:?}"
                    ))
                })?;
            sequence.push((kc, needs_shift));
        }
        if sequence.is_empty() {
            return Err(UiError::Internal("empty key chord".into()).into());
        }

        let shift_kc = lookup_keycode_for_keysym(&map.keysyms, syms_per_kc, min_kc, 0xFFE1).map(|(kc, _)| kc);
        let mut guard = InputReleaseGuard::new(|kc| fake_key(conn, false, kc));

        for (kc, needs_shift) in &sequence {
            if *needs_shift {
                if let Some(sk) = shift_kc {
                    if !already_pressed(&sequence, sk) {
                        fake_key(conn, true, sk);
                        guard.push(sk);
                    }
                }
            }
            fake_key(conn, true, *kc);
            guard.push(*kc);
        }

        std::thread::sleep(Duration::from_millis(8));
        guard.release_all();
        Ok(())
    })
}

/// Attempts to type ASCII characters directly via XTEST; returns `Err` for
/// non-ASCII UTF-8 or unmapped keysyms so caller can fall back to clipboard/IME.
pub fn type_ascii_text(text: &str) -> Result<()> {
    if !text.is_ascii() {
        return Err(UiError::Platform("non-ASCII text requires clipboard/xdotool path".into()).into());
    }
    with_xtest(|conn, _root| {
        let setup = conn.setup();
        let min_kc = setup.min_keycode;
        let count = setup.max_keycode.saturating_sub(min_kc).saturating_add(1);
        let map = conn
            .get_keyboard_mapping(min_kc, count)
            .map_err(|e| UiError::Platform(e.to_string()))?
            .reply()
            .map_err(|e| UiError::Platform(e.to_string()))?;
        let syms_per_kc = usize::from(map.keysyms_per_keycode.max(1));
        let shift_kc = lookup_keycode_for_keysym(&map.keysyms, syms_per_kc, min_kc, 0xFFE1).map(|(k, _)| k);

        let mut planned = Vec::with_capacity(text.len());
        for ch in text.chars() {
            let sym = ascii_char_to_keysym(ch).ok_or_else(|| {
                UiError::Platform(format!("unsupported ASCII control char {ch:?}"))
            })?;
            let entry = lookup_keycode_for_keysym(&map.keysyms, syms_per_kc, min_kc, sym)
                .ok_or_else(|| UiError::Platform(format!("keysym {sym:#x} missing from keymap")))?;
            planned.push(entry);
        }

        for (kc, needs_shift) in planned {
            let mut guard = InputReleaseGuard::new(|k| fake_key(conn, false, k));
            if needs_shift {
                if let Some(sk) = shift_kc {
                    fake_key(conn, true, sk);
                    guard.push(sk);
                }
            }
            fake_key(conn, true, kc);
            guard.push(kc);
            guard.release_all();
        }
        Ok(())
    })
}

fn already_pressed(seq: &[(u8, bool)], kc: u8) -> bool {
    seq.iter().any(|(k, _)| *k == kc)
}

fn lookup_keycode_for_keysym(
    table: &[u32],
    syms_per_kc: usize,
    min_kc: u8,
    target_sym: u32,
) -> Option<(u8, bool)> {
    // Column 0 = unshifted, Column 1 = shifted.
    for (idx, chunk) in table.chunks(syms_per_kc).enumerate() {
        let kc = min_kc.checked_add(idx as u8)?;
        if chunk.first().copied() == Some(target_sym) {
            return Some((kc, false));
        }
        if chunk.get(1).copied() == Some(target_sym) {
            return Some((kc, true));
        }
    }
    None
}

fn ascii_char_to_keysym(ch: char) -> Option<u32> {
    match ch {
        '\n' | '\r' => Some(0xFF0D),
        '\t' => Some(0xFF09),
        ' '..='~' => Some(ch as u32),
        _ => None,
    }
}

pub fn token_to_x11_keysym(token: &str) -> Result<u32> {
    let sym = match token {
        "Return" => 0xFF0D,
        "Tab" => 0xFF09,
        "Escape" => 0xFF1B,
        "space" => 0x0020,
        "BackSpace" => 0xFF08,
        "Delete" => 0xFFFF,
        "Insert" => 0xFF63,
        "Home" => 0xFF50,
        "End" => 0xFF57,
        "Prior" => 0xFF55,
        "Next" => 0xFF56,
        "Up" => 0xFF52,
        "Down" => 0xFF54,
        "Left" => 0xFF51,
        "Right" => 0xFF53,
        "ctrl" | "Control_L" => 0xFFE3,
        "alt" | "Alt_L" => 0xFFE9,
        "shift" | "Shift_L" => 0xFFE1,
        "super" | "Super_L" => 0xFFEB,
        "F1" => 0xFFBE,
        "F2" => 0xFFBF,
        "F3" => 0xFFC0,
        "F4" => 0xFFC1,
        "F5" => 0xFFC2,
        "F6" => 0xFFC3,
        "F7" => 0xFFC4,
        "F8" => 0xFFC5,
        "F9" => 0xFFC6,
        "F10" => 0xFFC7,
        "F11" => 0xFFC8,
        "F12" => 0xFFC9,
        s if s.chars().count() == 1 => {
            let c = s.chars().next().unwrap();
            ascii_char_to_keysym(c).ok_or_else(|| {
                UiError::Platform(format!("unsupported keysym char {c:?} for X11 XTEST"))
            })?
        }
        other => {
            return Err(
                UiError::Platform(format!("unsupported keysym {other:?} for X11 XTEST")).into(),
            );
        }
    };
    Ok(sym)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_unshifted_and_shifted_keymap_columns() {
        // Fake table: 2 keycodes starting at min_kc=8, 2 syms per keycode
        // kc=8: ['a', 'A'], kc=9: ['1', '!']
        let table = vec![b'a' as u32, b'A' as u32, b'1' as u32, b'!' as u32];
        assert_eq!(
            lookup_keycode_for_keysym(&table, 2, 8, b'a' as u32),
            Some((8, false))
        );
        assert_eq!(
            lookup_keycode_for_keysym(&table, 2, 8, b'A' as u32),
            Some((8, true))
        );
        assert_eq!(
            lookup_keycode_for_keysym(&table, 2, 8, b'!' as u32),
            Some((9, true))
        );
        assert_eq!(lookup_keycode_for_keysym(&table, 2, 8, 0xFF0D), None);
    }
}
