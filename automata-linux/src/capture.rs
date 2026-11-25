use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use image::{DynamicImage, RgbaImage};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    AtomEnum, ConnectionExt as _, ImageFormat, ImageOrder, MapState, Window,
};
use x11rb::rust_connection::RustConnection;

use crate::a11y::A11yClient;
use crate::backend::{ConnectionMode, DEFAULT_SCREENCAST_IDLE_TIMEOUT, resolve_connection_mode};
use crate::{Result, UiError};

struct CachedX11Conn {
    conn: RustConnection,
    screen_num: usize,
    pid_atom: Option<u32>,
    utf8_atom: Option<u32>,
    net_wm_name: Option<u32>,
    net_client_list_stacking: Option<u32>,
    net_client_list: Option<u32>,
}

struct CachedScreenCastSession {
    conn: zbus::Connection,
    session_path: zbus::zvariant::OwnedObjectPath,
    node_id: u32,
    last_used: Instant,
}

static PERSISTENT_X11: OnceLock<Mutex<Option<CachedX11Conn>>> = OnceLock::new();
static PERSISTENT_SCREENCAST: OnceLock<Mutex<Option<CachedScreenCastSession>>> = OnceLock::new();
static X11_SESSION_DEPTH: AtomicUsize = AtomicUsize::new(0);
static SCREENCAST_SESSION_DEPTH: AtomicUsize = AtomicUsize::new(0);
static SCREENCAST_IDLE_TIMEOUT_SECS: AtomicU64 =
    AtomicU64::new(DEFAULT_SCREENCAST_IDLE_TIMEOUT.as_secs());
static SCREENCAST_REAPER_RUNNING: AtomicBool = AtomicBool::new(false);

fn x11_pool() -> &'static Mutex<Option<CachedX11Conn>> {
    PERSISTENT_X11.get_or_init(|| Mutex::new(None))
}

fn screencast_pool() -> &'static Mutex<Option<CachedScreenCastSession>> {
    PERSISTENT_SCREENCAST.get_or_init(|| Mutex::new(None))
}

/// RAII scope guard that keeps X11 socket/atom caches (and optionally ScreenCast
/// Portal sessions) alive for the duration of a workflow or daemon scope.
pub struct CaptureSessionGuard {
    keep_x11: bool,
    keep_screencast: bool,
}

pub(crate) fn enter_capture_session(
    keep_x11: bool,
    screencast_idle_timeout: Option<Duration>,
) -> CaptureSessionGuard {
    if keep_x11 {
        X11_SESSION_DEPTH.fetch_add(1, Ordering::AcqRel);
    }
    let keep_screencast = screencast_idle_timeout.is_some();
    if let Some(timeout) = screencast_idle_timeout {
        SCREENCAST_IDLE_TIMEOUT_SECS.store(timeout.as_secs().max(1), Ordering::Release);
        SCREENCAST_SESSION_DEPTH.fetch_add(1, Ordering::AcqRel);
    }
    CaptureSessionGuard {
        keep_x11,
        keep_screencast,
    }
}

impl Drop for CaptureSessionGuard {
    fn drop(&mut self) {
        if self.keep_x11 && X11_SESSION_DEPTH.fetch_sub(1, Ordering::AcqRel) == 1 {
            if !x11_keepalive_enabled() {
                *x11_pool().lock().unwrap_or_else(|e| e.into_inner()) = None;
            }
        }
        if self.keep_screencast && SCREENCAST_SESSION_DEPTH.fetch_sub(1, Ordering::AcqRel) == 1 {
            if screencast_keepalive_timeout().is_none() {
                close_cached_screencast();
            }
        }
    }
}

pub(crate) fn close_persistent_captures() {
    *x11_pool().lock().unwrap_or_else(|e| e.into_inner()) = None;
    close_cached_screencast();
}

fn x11_keepalive_enabled() -> bool {
    X11_SESSION_DEPTH.load(Ordering::Acquire) > 0
        || matches!(
            resolve_connection_mode(ConnectionMode::OneShot),
            Ok(ConnectionMode::Persistent { .. })
        )
}

fn screencast_keepalive_timeout() -> Option<Duration> {
    if SCREENCAST_SESSION_DEPTH.load(Ordering::Acquire) > 0 {
        let secs = SCREENCAST_IDLE_TIMEOUT_SECS.load(Ordering::Acquire).max(1);
        return Some(Duration::from_secs(secs));
    }
    match resolve_connection_mode(ConnectionMode::OneShot) {
        Ok(ConnectionMode::Persistent {
            screencast_idle_timeout,
        }) => Some(screencast_idle_timeout),
        _ => None,
    }
}

#[derive(Debug, Clone)]
pub(crate) struct X11WindowMatch {
    pub xid: Window,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Debug, Clone)]
pub(crate) struct X11WindowMeta {
    pub pid: Option<u32>,
    pub title: Option<String>,
    pub rect: (i32, i32, i32, i32),
}

#[derive(Debug, Clone)]
pub(crate) struct AtspiWindowMeta {
    pub hwnd: u64,
    pub pid: Option<u32>,
    pub title: Option<String>,
    pub bounds: Option<(i32, i32, i32, i32)>,
}

/// Decode X11 `ZPixmap` pixel buffer into an opaque `RgbaImage`.
///
/// Only touches pixel rows inside `width x height`; forcing alpha=255 avoids
/// undefined padding bits on depth-24 windows turning OCR regions transparent.
pub(crate) fn decode_zpixmap_rgba(
    data: &[u8],
    width: u32,
    height: u32,
    depth: u8,
    bits_per_pixel: u8,
    byte_order: ImageOrder,
) -> Result<RgbaImage> {
    let pixel_count = (width as usize)
        .checked_mul(height as usize)
        .ok_or_else(|| UiError::Internal("captured image dimensions overflow".into()))?;
    let mut out = vec![0u8; pixel_count * 4];

    match (depth, bits_per_pixel) {
        (24 | 32, 32) => {
            if data.len() < pixel_count * 4 {
                return Err(UiError::Platform(format!(
                    "x11rb get_image truncated: expected {} bytes, got {}",
                    pixel_count * 4,
                    data.len()
                ))
                .into());
            }
            let lsb = byte_order == ImageOrder::LSB_FIRST;
            for (dst, src) in out.chunks_exact_mut(4).zip(data.chunks_exact(4)) {
                if lsb {
                    // BGRA / BGRX in memory
                    dst[0] = src[2];
                    dst[1] = src[1];
                    dst[2] = src[0];
                    dst[3] = 255;
                } else {
                    // XRGB / ARGB in memory
                    dst[0] = src[1];
                    dst[1] = src[2];
                    dst[2] = src[3];
                    dst[3] = 255;
                }
            }
        }
        (24, 24) => {
            if data.len() < pixel_count * 3 {
                return Err(UiError::Platform(format!(
                    "x11rb 24bpp image truncated: expected {} bytes, got {}",
                    pixel_count * 3,
                    data.len()
                ))
                .into());
            }
            let lsb = byte_order == ImageOrder::LSB_FIRST;
            for (dst, src) in out.chunks_exact_mut(4).zip(data.chunks_exact(3)) {
                if lsb {
                    dst[0] = src[2];
                    dst[1] = src[1];
                    dst[2] = src[0];
                } else {
                    dst[0] = src[0];
                    dst[1] = src[1];
                    dst[2] = src[2];
                }
                dst[3] = 255;
            }
        }
        (16, 16) => {
            if data.len() < pixel_count * 2 {
                return Err(UiError::Platform(format!(
                    "x11rb 16bpp image truncated: expected {} bytes, got {}",
                    pixel_count * 2,
                    data.len()
                ))
                .into());
            }
            let lsb = byte_order == ImageOrder::LSB_FIRST;
            for (dst, src) in out.chunks_exact_mut(4).zip(data.chunks_exact(2)) {
                let px = if lsb {
                    u16::from_le_bytes([src[0], src[1]])
                } else {
                    u16::from_be_bytes([src[0], src[1]])
                };
                dst[0] = (((px >> 11) & 0x1f) * 255 / 31) as u8;
                dst[1] = (((px >> 5) & 0x3f) * 255 / 63) as u8;
                dst[2] = ((px & 0x1f) * 255 / 31) as u8;
                dst[3] = 255;
            }
        }
        _ => {
            return Err(UiError::Platform(format!(
                "unsupported X11 image format: depth={depth} bpp={bits_per_pixel}"
            ))
            .into());
        }
    }

    RgbaImage::from_raw(width, height, out)
        .ok_or_else(|| UiError::Internal("failed to construct RgbaImage".into()).into())
}

fn connect_x11() -> Result<CachedX11Conn> {
    let (conn, screen_num) = RustConnection::connect(None).map_err(|e| {
        UiError::Platform(format!("x11rb failed to connect to X11 DISPLAY: {e}"))
    })?;
    let pid_atom = intern_atom(&conn, b"_NET_WM_PID");
    let utf8_atom = intern_atom(&conn, b"UTF8_STRING");
    let net_wm_name = intern_atom(&conn, b"_NET_WM_NAME");
    let net_client_list_stacking = intern_atom(&conn, b"_NET_CLIENT_LIST_STACKING");
    let net_client_list = intern_atom(&conn, b"_NET_CLIENT_LIST");
    Ok(CachedX11Conn {
        conn,
        screen_num,
        pid_atom,
        utf8_atom,
        net_wm_name,
        net_client_list_stacking,
        net_client_list,
    })
}

/// Execute `op` against an X11 connection.
///
/// In one-shot mode, opens and drops a fresh connection; in persistent/session mode,
/// reuses the cached connection + interned atoms and automatically reconnects once on error.
fn with_x11_conn<T, F>(mut op: F) -> Result<T>
where
    F: FnMut(&CachedX11Conn) -> Result<T>,
{
    if !x11_keepalive_enabled() {
        let fresh = connect_x11()?;
        return op(&fresh);
    }

    let mut guard = x11_pool().lock().unwrap_or_else(|e| e.into_inner());
    let reused = guard.is_some();
    if guard.is_none() {
        *guard = Some(connect_x11()?);
    }
    let res = op(guard.as_ref().expect("just initialized"));
    match res {
        Ok(v) => Ok(v),
        Err(first_err) if reused => {
            log::debug!("reconnecting stale persistent X11 connection: {first_err}");
            *guard = Some(connect_x11()?);
            op(guard.as_ref().expect("just reconnected"))
        }
        Err(err) => {
            *guard = None;
            Err(err)
        }
    }
}

fn intern_atom(conn: &RustConnection, name: &[u8]) -> Option<u32> {
    conn.intern_atom(false, name)
        .ok()?
        .reply()
        .ok()
        .map(|r| r.atom)
        .filter(|&a| a != x11rb::NONE)
}

fn window_pid(conn: &RustConnection, pid_atom: u32, xid: Window) -> Option<u32> {
    let reply = conn
        .get_property(false, xid, pid_atom, AtomEnum::CARDINAL, 0, 1)
        .ok()?
        .reply()
        .ok()?;
    reply.value32()?.next()
}

fn window_title(conn: &RustConnection, utf8_atom: Option<u32>, net_wm_name: Option<u32>, xid: Window) -> Option<String> {
    if let (Some(prop), Some(ty)) = (net_wm_name, utf8_atom) {
        if let Ok(cookie) = conn.get_property(false, xid, prop, ty, 0, 1024) {
            if let Ok(reply) = cookie.reply() {
                if !reply.value.is_empty() {
                    return Some(String::from_utf8_lossy(&reply.value).into_owned());
                }
            }
        }
    }
    let reply = conn
        .get_property(false, xid, AtomEnum::WM_NAME, AtomEnum::ANY, 0, 1024)
        .ok()?
        .reply()
        .ok()?;
    if reply.value.is_empty() {
        None
    } else {
        Some(String::from_utf8_lossy(&reply.value).into_owned())
    }
}

fn window_screen_rect(conn: &RustConnection, root: Window, xid: Window) -> Option<(i32, i32, i32, i32)> {
    let attrs = conn.get_window_attributes(xid).ok()?.reply().ok()?;
    if attrs.map_state != MapState::VIEWABLE {
        return None;
    }
    let geom = conn.get_geometry(xid).ok()?.reply().ok()?;
    if geom.width == 0 || geom.height == 0 {
        return None;
    }
    let tr = conn
        .translate_coordinates(xid, root, 0, 0)
        .ok()?
        .reply()
        .ok()?;
    Some((
        i32::from(tr.dst_x),
        i32::from(tr.dst_y),
        i32::from(geom.width),
        i32::from(geom.height),
    ))
}

fn read_root_window_list(conn: &RustConnection, root: Window, atom: Option<u32>) -> Vec<Window> {
    let mut ids = Vec::new();
    if let Some(atom) = atom {
        if let Ok(cookie) = conn.get_property(false, root, atom, AtomEnum::WINDOW, 0, 4096) {
            if let Ok(reply) = cookie.reply() {
                if let Some(vals) = reply.value32() {
                    ids.extend(vals);
                }
            }
        }
    }
    ids
}

/// Top-level X11 windows ordered topmost-first per EWMH `_NET_CLIENT_LIST_STACKING`
/// (which is bottom-to-top) or X11 `query_tree(root)` fallback (also bottom-to-top).
fn stacking_x11_windows_top_to_bottom(ctx: &CachedX11Conn, root: Window) -> Vec<Window> {
    let mut ids = read_root_window_list(&ctx.conn, root, ctx.net_client_list_stacking);
    if ids.is_empty() {
        if let Ok(cookie) = ctx.conn.query_tree(root) {
            if let Ok(reply) = cookie.reply() {
                ids.extend(reply.children);
            }
        }
    }
    ids.reverse();
    ids
}

fn candidate_x11_windows(ctx: &CachedX11Conn, root: Window) -> Vec<Window> {
    let ids = stacking_x11_windows_top_to_bottom(ctx, root);
    if !ids.is_empty() {
        return ids;
    }
    read_root_window_list(&ctx.conn, root, ctx.net_client_list)
}

fn score_x11_window_match(
    rect: (i32, i32, i32, i32),
    got_title: Option<&str>,
    want_title: Option<&str>,
    atspi_bounds: Option<(i32, i32, i32, i32)>,
) -> i64 {
    let (wx, wy, ww, wh) = rect;
    let mut score: i64 = 0;
    if let Some((ax, ay, aw, ah)) = atspi_bounds {
        score += i64::from((ww - aw).abs()) * 4
            + i64::from((wh - ah).abs()) * 4
            + i64::from((wx - ax).abs())
            + i64::from((wy - ay).abs());
    }
    if let (Some(want), Some(got)) = (
        want_title.map(str::trim).filter(|s| !s.is_empty()),
        got_title.map(str::trim).filter(|s| !s.is_empty()),
    ) {
        if got == want {
            score -= 1000;
        } else if got.contains(want) || want.contains(got) {
            score -= 300;
        }
    }
    score
}

/// Enumerate viewable X11 windows ordered topmost-first (Z-order head at index 0).
pub(crate) fn x11_topmost_windows() -> Result<Vec<X11WindowMeta>> {
    with_x11_conn(|ctx| {
        let root = ctx.conn.setup().roots[ctx.screen_num].root;
        let mut out = Vec::new();
        for xid in stacking_x11_windows_top_to_bottom(ctx, root) {
            let Some(rect) = window_screen_rect(&ctx.conn, root, xid) else {
                continue;
            };
            let pid = ctx.pid_atom.and_then(|atom| window_pid(&ctx.conn, atom, xid));
            let title = window_title(&ctx.conn, ctx.utf8_atom, ctx.net_wm_name, xid);
            out.push(X11WindowMeta { pid, title, rect });
        }
        Ok(out)
    })
}

/// Map topmost-first X11 windows onto AT-SPI2 synthetic HWNDs using optimal 1-to-1
/// similarity matching, then sort the matched HWNDs by their X11 stacking rank.
pub(crate) fn match_z_order_to_hwnds(
    x11_top_to_bottom: &[X11WindowMeta],
    atspi_windows: &[AtspiWindowMeta],
) -> Vec<u64> {
    if x11_top_to_bottom.is_empty() || atspi_windows.is_empty() {
        return Vec::new();
    }

    // Collect candidate (score, x11_rank, atspi_idx) pairs.
    let mut pairs: Vec<(i64, usize, usize)> = Vec::new();
    for (x11_rank, xwin) in x11_top_to_bottom.iter().enumerate() {
        for (atspi_idx, awin) in atspi_windows.iter().enumerate() {
            let pid_compatible = match (xwin.pid, awin.pid) {
                (Some(xp), Some(ap)) => xp == ap,
                _ => {
                    // Without PIDs on both sides, only pair windows that share a non-empty title.
                    matches!(
                        (xwin.title.as_deref(), awin.title.as_deref()),
                        (Some(xt), Some(at)) if !xt.trim().is_empty() && !at.trim().is_empty() && (xt == at || xt.contains(at) || at.contains(xt))
                    )
                }
            };
            if !pid_compatible {
                continue;
            }
            let score = score_x11_window_match(
                xwin.rect,
                xwin.title.as_deref(),
                awin.title.as_deref(),
                awin.bounds,
            );
            pairs.push((score, x11_rank, atspi_idx));
        }
    }

    pairs.sort_by_key(|&(score, x11_rank, atspi_idx)| (score, x11_rank, atspi_idx));

    let mut x11_used = vec![false; x11_top_to_bottom.len()];
    let mut atspi_used = vec![false; atspi_windows.len()];
    let mut matched: Vec<(usize, u64)> = Vec::new();

    for (_score, x11_rank, atspi_idx) in pairs {
        if x11_used[x11_rank] || atspi_used[atspi_idx] {
            continue;
        }
        x11_used[x11_rank] = true;
        atspi_used[atspi_idx] = true;
        matched.push((x11_rank, atspi_windows[atspi_idx].hwnd));
    }

    matched.sort_by_key(|&(x11_rank, _)| x11_rank);
    matched.into_iter().map(|(_, hwnd)| hwnd).collect()
}

fn resolve_x11_window(
    ctx: &CachedX11Conn,
    root: Window,
    pid: Option<u32>,
    title: Option<&str>,
    atspi_bounds: Option<(i32, i32, i32, i32)>,
) -> Option<X11WindowMatch> {
    let pid_atom = ctx.pid_atom?;
    let target_pid = pid?;
    let candidates = candidate_x11_windows(ctx, root);

    let mut best: Option<(i64, X11WindowMatch)> = None;
    for xid in candidates {
        if window_pid(&ctx.conn, pid_atom, xid) != Some(target_pid) {
            continue;
        }
        let Some((wx, wy, ww, wh)) = window_screen_rect(&ctx.conn, root, xid) else {
            continue;
        };
        let got_title = window_title(&ctx.conn, ctx.utf8_atom, ctx.net_wm_name, xid);
        let score = score_x11_window_match((wx, wy, ww, wh), got_title.as_deref(), title, atspi_bounds);
        let m = X11WindowMatch {
            xid,
            x: wx,
            y: wy,
            width: ww,
            height: wh,
        };
        if best.as_ref().is_none_or(|(best_score, _)| score < *best_score) {
            best = Some((score, m));
        }
    }
    best.map(|(_, m)| m)
}

fn capture_drawable_rgba(
    conn: &RustConnection,
    drawable: Window,
    req_x: i32,
    req_y: i32,
    req_w: i32,
    req_h: i32,
) -> Result<DynamicImage> {
    let geom = conn
        .get_geometry(drawable)
        .map_err(|e| UiError::Platform(format!("x11rb get_geometry send failed: {e}")))?
        .reply()
        .map_err(|e| UiError::Platform(format!("x11rb get_geometry reply failed: {e}")))?;

    let max_w = i32::from(geom.width);
    let max_h = i32::from(geom.height);
    let cx = req_x.clamp(0, max_w.saturating_sub(1));
    let cy = req_y.clamp(0, max_h.saturating_sub(1));
    let cw = if req_w <= 0 {
        max_w - cx
    } else {
        req_w.min(max_w - cx)
    };
    let ch = if req_h <= 0 {
        max_h - cy
    } else {
        req_h.min(max_h - cy)
    };
    if cw <= 0 || ch <= 0 {
        return Err(UiError::Internal(format!(
            "x11rb capture region is empty after clamping ({cx},{cy} {cw}x{ch} inside {max_w}x{max_h})"
        ))
        .into());
    }

    let reply = conn
        .get_image(
            ImageFormat::Z_PIXMAP,
            drawable,
            cx as i16,
            cy as i16,
            cw as u16,
            ch as u16,
            u32::MAX,
        )
        .map_err(|e| UiError::Platform(format!("x11rb get_image send failed: {e}")))?
        .reply()
        .map_err(|e| UiError::Platform(format!("x11rb get_image reply failed: {e}")))?;

    let setup = conn.setup();
    let bpp = setup
        .pixmap_formats
        .iter()
        .find(|f| f.depth == reply.depth)
        .map(|f| f.bits_per_pixel)
        .unwrap_or(32);

    let rgba = decode_zpixmap_rgba(
        &reply.data,
        cw as u32,
        ch as u32,
        reply.depth,
        bpp,
        setup.image_byte_order,
    )?;
    Ok(DynamicImage::ImageRgba8(rgba))
}

/// Capture the full X11 screen via `x11rb` (`ZPixmap` `GetImage`).
pub(crate) fn capture_x11rb_screen() -> Result<DynamicImage> {
    with_x11_conn(|ctx| {
        let screen = &ctx.conn.setup().roots[ctx.screen_num];
        capture_drawable_rgba(
            &ctx.conn,
            screen.root,
            0,
            0,
            i32::from(screen.width_in_pixels),
            i32::from(screen.height_in_pixels),
        )
    })
}

/// Capture a screen-space rectangle `(x, y, width, height)` via `x11rb`.
pub(crate) fn capture_x11rb_region(x: i32, y: i32, width: i32, height: i32) -> Result<DynamicImage> {
    with_x11_conn(|ctx| {
        let root = ctx.conn.setup().roots[ctx.screen_num].root;
        capture_drawable_rgba(&ctx.conn, root, x, y, width, height)
    })
}

/// Computes the non-empty intersection of two `(x, y, width, height)` rectangles.
pub fn intersect_rect(
    a: (i32, i32, i32, i32),
    b: (i32, i32, i32, i32),
) -> Option<(i32, i32, i32, i32)> {
    let x1 = a.0.max(b.0);
    let y1 = a.1.max(b.1);
    let x2 = (a.0 + a.2).min(b.0 + b.2);
    let y2 = (a.1 + a.3).min(b.1 + b.3);
    let w = x2 - x1;
    let h = y2 - y1;
    (w > 0 && h > 0).then_some((x1, y1, w, h))
}

/// Returns `true` when multi-monitor logical rects leave unbacked dead area
/// inside their bounding box (where raw root-window `GetImage` can trigger `BadMatch`).
pub fn monitors_have_gap(monitors: &[(i32, i32, i32, i32)]) -> bool {
    if monitors.len() <= 1 {
        return false;
    }
    let min_x = monitors.iter().map(|m| m.0).min().unwrap_or(0);
    let min_y = monitors.iter().map(|m| m.1).min().unwrap_or(0);
    let max_x = monitors.iter().map(|m| m.0 + m.2).max().unwrap_or(0);
    let max_y = monitors.iter().map(|m| m.1 + m.3).max().unwrap_or(0);
    let bbox_area = i64::from((max_x - min_x).max(0)) * i64::from((max_y - min_y).max(0));
    let total_monitor_area: i64 = monitors
        .iter()
        .map(|m| i64::from(m.2.max(0)) * i64::from(m.3.max(0)))
        .sum();
    total_monitor_area < bbox_area
}

/// Capture an X11 or XWayland window (or a screen-coordinate sub-region inside it) via `x11rb`.
///
/// Clamps off-screen edges cleanly to avoid X11 `BadMatch` on multi-monitor setups, and
/// returns `Ok(None)` when `DISPLAY` cannot be contacted or no matching X11 window exists
/// (e.g., for native Wayland clients), allowing `auto` mode to fall back cleanly.
pub(crate) fn capture_x11rb_window(
    pid: Option<u32>,
    title: Option<&str>,
    atspi_bounds: Option<(i32, i32, i32, i32)>,
    screen_region: Option<(i32, i32, i32, i32)>,
) -> Result<Option<(DynamicImage, i32, i32)>> {
    let res = with_x11_conn(|ctx| {
        let screen = &ctx.conn.setup().roots[ctx.screen_num];
        let root = screen.root;
        let screen_rect = (
            0,
            0,
            i32::from(screen.width_in_pixels),
            i32::from(screen.height_in_pixels),
        );
        let Some(win) = resolve_x11_window(ctx, root, pid, title, atspi_bounds) else {
            return Ok(None);
        };

        // Map optional screen-space crop into window-local coordinates.
        let (anchor_x, anchor_y) = match atspi_bounds {
            Some((ax, ay, _, _)) if win.x == 0 && win.y == 0 && (ax != 0 || ay != 0) => (ax, ay),
            _ => (win.x, win.y),
        };
        let (local_x, local_y, req_w, req_h, out_ox, out_oy) = match screen_region {
            Some((rx, ry, rw, rh)) => (rx - anchor_x, ry - anchor_y, rw, rh, rx, ry),
            None => (0, 0, win.width, win.height, anchor_x, anchor_y),
        };

        match capture_drawable_rgba(&ctx.conn, win.xid, local_x, local_y, req_w, req_h) {
            Ok(img) => Ok(Some((img, out_ox, out_oy))),
            Err(_) => {
                // Multi-monitor edge / offscreen BadMatch fallback: intersect desired screen
                // rectangle with root screen dimensions and capture from root.
                let desired = (out_ox, out_oy, req_w, req_h);
                let clipped = intersect_rect(desired, screen_rect).ok_or_else(|| {
                    UiError::Internal("target window rectangle lies completely off-screen".into())
                })?;
                let img = capture_drawable_rgba(
                    &ctx.conn,
                    root,
                    clipped.0,
                    clipped.1,
                    clipped.2,
                    clipped.3,
                )?;
                Ok(Some((img, clipped.0, clipped.1)))
            }
        }
    });
    match res {
        Ok(v) => Ok(v),
        // Graceful None when X11 DISPLAY itself cannot be reached so Wayland fallback stays clean.
        Err(err) if err.to_string().contains("x11rb failed to connect") => Ok(None),
        Err(err) => Err(err),
    }
}

fn screencast_token_path() -> PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("ui-automata").join("screencast_restore_token")
}

fn load_screencast_token() -> Option<String> {
    std::fs::read_to_string(screencast_token_path())
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn save_screencast_token(token: &str) {
    if token.trim().is_empty() {
        return;
    }
    let path = screencast_token_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, token.trim());
}

async fn wait_portal_response(
    conn: &zbus::Connection,
    req_path: &str,
) -> std::result::Result<HashMap<String, zbus::zvariant::OwnedValue>, UiError> {
    use futures_util::StreamExt;

    let req_proxy = zbus::Proxy::new(
        conn,
        "org.freedesktop.portal.Desktop",
        req_path,
        "org.freedesktop.portal.Request",
    )
    .await
    .map_err(|e| UiError::Platform(format!("Portal Request proxy failed: {e}")))?;

    let mut stream = req_proxy
        .receive_signal("Response")
        .await
        .map_err(|e| UiError::Platform(format!("Portal Response listen failed: {e}")))?;

    let msg = tokio::time::timeout(std::time::Duration::from_secs(30), stream.next())
        .await
        .map_err(|_| UiError::Platform("Portal ScreenCast response timed out".into()))?
        .ok_or_else(|| UiError::Platform("Portal ScreenCast stream ended".into()))?;

    let (code, results): (u32, HashMap<String, zbus::zvariant::OwnedValue>) = msg
        .body()
        .deserialize()
        .map_err(|e| UiError::Platform(format!("Portal Response decode failed: {e}")))?;

    if code != 0 {
        return Err(UiError::Platform(format!(
            "Portal ScreenCast cancelled/denied (response={code})"
        )));
    }
    Ok(results)
}

static SCREENCAST_SEQ: AtomicU64 = AtomicU64::new(1);

async fn close_portal_session_async(
    conn: &zbus::Connection,
    session_path: &zbus::zvariant::OwnedObjectPath,
) {
    if let Ok(sess) = zbus::Proxy::new(
        conn,
        "org.freedesktop.portal.Desktop",
        session_path.as_str(),
        "org.freedesktop.portal.Session",
    )
    .await
    {
        let _: std::result::Result<(), _> = sess.call("Close", &()).await;
    }
}

fn close_screencast_session(session: CachedScreenCastSession) {
    if let Ok(client) = A11yClient::shared() {
        client.block_on(close_portal_session_async(&session.conn, &session.session_path));
    }
}

fn close_cached_screencast() {
    let taken = screencast_pool()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take();
    if let Some(sess) = taken {
        close_screencast_session(sess);
    }
}

fn ensure_screencast_reaper() {
    if SCREENCAST_REAPER_RUNNING
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    std::thread::spawn(|| {
        loop {
            std::thread::sleep(Duration::from_secs(2));
            let Some(timeout) = screencast_keepalive_timeout() else {
                close_cached_screencast();
                SCREENCAST_REAPER_RUNNING.store(false, Ordering::Release);
                break;
            };
            let expired = {
                let mut guard = screencast_pool().lock().unwrap_or_else(|e| e.into_inner());
                match guard.as_ref() {
                    None => {
                        SCREENCAST_REAPER_RUNNING.store(false, Ordering::Release);
                        break;
                    }
                    Some(sess) if sess.last_used.elapsed() >= timeout => guard.take(),
                    Some(_) => None,
                }
            };
            if let Some(sess) = expired {
                log::debug!("closing idle persistent ScreenCast session after {:?}", timeout);
                close_screencast_session(sess);
            }
        }
    });
}

async fn start_screencast_portal_session(
    saved_token: Option<&str>,
) -> std::result::Result<CachedScreenCastSession, UiError> {
    use zbus::zvariant::{OwnedObjectPath, Value};

    let conn = zbus::Connection::session()
        .await
        .map_err(|e| UiError::Platform(format!("session dbus failed: {e}")))?;
    let proxy = zbus::Proxy::new(
        &conn,
        "org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.ScreenCast",
    )
    .await
    .map_err(|e| UiError::Platform(format!("ScreenCast portal proxy failed: {e}")))?;

    let pid = std::process::id();
    let seq = SCREENCAST_SEQ.fetch_add(1, Ordering::Relaxed);
    let sess_token = format!("automata_sc_sess_{pid}_{seq}");
    let mut create_opts: HashMap<&str, Value<'_>> = HashMap::new();
    create_opts.insert(
        "handle_token",
        Value::from(format!("automata_sc_c_{pid}_{seq}")),
    );
    create_opts.insert("session_handle_token", Value::from(sess_token.as_str()));

    let req_path: OwnedObjectPath = proxy
        .call("CreateSession", &(create_opts,))
        .await
        .map_err(|e| UiError::Platform(format!("ScreenCast CreateSession failed: {e}")))?;
    let create_res = wait_portal_response(&conn, req_path.as_str()).await?;
    let session_handle: String = create_res
        .get("session_handle")
        .and_then(|v| v.downcast_ref::<&str>().ok())
        .map(|s| s.to_string())
        .ok_or_else(|| UiError::Platform("ScreenCast CreateSession missing session_handle".into()))?;
    let session_path = OwnedObjectPath::try_from(session_handle.as_str())
        .map_err(|e| UiError::Platform(format!("invalid session handle: {e}")))?;

    let mut select_opts: HashMap<&str, Value<'_>> = HashMap::new();
    select_opts.insert(
        "handle_token",
        Value::from(format!("automata_sc_s_{pid}_{seq}")),
    );
    select_opts.insert("types", Value::from(1u32 | 2u32)); // Monitor | Window
    select_opts.insert("multiple", Value::from(false));
    select_opts.insert("persist_mode", Value::from(2u32));
    if let Some(tok) = saved_token {
        select_opts.insert("restore_token", Value::from(tok));
    }

    let sel_req: OwnedObjectPath = proxy
        .call("SelectSources", &(&session_path, select_opts))
        .await
        .map_err(|e| UiError::Platform(format!("ScreenCast SelectSources failed: {e}")))?;
    wait_portal_response(&conn, sel_req.as_str()).await?;

    let mut start_opts: HashMap<&str, Value<'_>> = HashMap::new();
    start_opts.insert(
        "handle_token",
        Value::from(format!("automata_sc_go_{pid}_{seq}")),
    );
    let start_req: OwnedObjectPath = proxy
        .call("Start", &(&session_path, "", start_opts))
        .await
        .map_err(|e| UiError::Platform(format!("ScreenCast Start failed: {e}")))?;
    let start_res = wait_portal_response(&conn, start_req.as_str()).await?;

    if let Some(tok) = start_res
        .get("restore_token")
        .and_then(|v| v.downcast_ref::<&str>().ok())
    {
        save_screencast_token(tok);
    }

    let streams_val = start_res
        .get("streams")
        .ok_or_else(|| UiError::Platform("ScreenCast Start returned no streams".into()))?;
    let streams: Vec<(u32, HashMap<String, zbus::zvariant::OwnedValue>)> = streams_val
        .clone()
        .try_into()
        .map_err(|e| UiError::Platform(format!("ScreenCast streams decode failed: {e}")))?;
    let node_id = streams
        .first()
        .map(|(id, _)| *id)
        .ok_or_else(|| UiError::Platform("ScreenCast returned empty stream list".into()))?;

    Ok(CachedScreenCastSession {
        conn,
        session_path,
        node_id,
        last_used: Instant::now(),
    })
}

async fn open_pipewire_remote_async(
    sess: &CachedScreenCastSession,
) -> std::result::Result<zbus::zvariant::OwnedFd, UiError> {
    use zbus::zvariant::{OwnedFd, Value};

    let proxy = zbus::Proxy::new(
        &sess.conn,
        "org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.ScreenCast",
    )
    .await
    .map_err(|e| UiError::Platform(format!("ScreenCast portal proxy failed: {e}")))?;

    let empty_opts: HashMap<&str, Value<'_>> = HashMap::new();
    let fd: OwnedFd = proxy
        .call("OpenPipeWireRemote", &(&sess.session_path, empty_opts))
        .await
        .map_err(|e| UiError::Platform(format!("ScreenCast OpenPipeWireRemote failed: {e}")))?;
    Ok(fd)
}

fn run_gst_screencast_frame(
    pw_fd: zbus::zvariant::OwnedFd,
    node_id: u32,
    out_path: &Path,
) -> Result<()> {
    let std_fd: std::os::fd::OwnedFd = pw_fd.into();
    let pipe_file = std::fs::File::from(std_fd);
    let pipeline_out = out_path.to_str().unwrap_or("");
    let node_arg = format!("path={node_id}");

    let status = Command::new("gst-launch-1.0")
        .args([
            "-q",
            "pipewiresrc",
            "fd=0",
            &node_arg,
            "num-buffers=1",
            "!",
            "videoconvert",
            "!",
            "pngenc",
            "!",
            "filesink",
            &format!("location={pipeline_out}"),
        ])
        .stdin(Stdio::from(pipe_file))
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| UiError::Platform(format!("failed to execute gst-launch-1.0: {e}")))?;

    if !status.status.success() {
        let err = String::from_utf8_lossy(&status.stderr);
        return Err(UiError::Platform(format!(
            "gst-launch-1.0 pipewiresrc failed: {}",
            err.trim()
        ))
        .into());
    }
    Ok(())
}

/// Capture one frame through `org.freedesktop.portal.ScreenCast` + PipeWire (`gst-launch-1.0`).
///
/// Persists the portal `restore_token` under `~/.cache/ui-automata/screencast_restore_token`
/// (`persist_mode = 2`). In `OneShot` mode the portal session closes immediately after the frame;
/// in `Persistent` mode the portal session stays warm across frames until idle timeout or drop.
pub(crate) fn capture_via_screencast(out_path: &Path) -> Result<()> {
    if let Some(parent) = out_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    let client = A11yClient::shared()?;
    let keepalive = screencast_keepalive_timeout();

    if keepalive.is_none() {
        let saved_token = load_screencast_token();
        let (sess, fd) = client.block_on(async {
            let sess = start_screencast_portal_session(saved_token.as_deref()).await?;
            let fd = open_pipewire_remote_async(&sess).await?;
            Ok::<_, UiError>((sess, fd))
        })?;
        let node_id = sess.node_id;
        let res = run_gst_screencast_frame(fd, node_id, out_path);
        close_screencast_session(sess);
        return res;
    }

    let timeout = keepalive.expect("checked Some");
    // Try cached session first; fall back once on stale session/stream failure.
    for attempt in 0..2 {
        let existing = {
            let mut guard = screencast_pool().lock().unwrap_or_else(|e| e.into_inner());
            match guard.take() {
                Some(sess) if sess.last_used.elapsed() >= timeout => {
                    close_screencast_session(sess);
                    None
                }
                other => other,
            }
        };

        let is_reused = existing.is_some();
        let (mut sess, fd) = match existing {
            Some(s) => match client.block_on(open_pipewire_remote_async(&s)) {
                Ok(fd) => (s, fd),
                Err(_) => {
                    close_screencast_session(s);
                    let saved_token = load_screencast_token();
                    client.block_on(async {
                        let fresh = start_screencast_portal_session(saved_token.as_deref()).await?;
                        let fd = open_pipewire_remote_async(&fresh).await?;
                        Ok::<_, UiError>((fresh, fd))
                    })?
                }
            },
            None => {
                let saved_token = load_screencast_token();
                client.block_on(async {
                    let fresh = start_screencast_portal_session(saved_token.as_deref()).await?;
                    let fd = open_pipewire_remote_async(&fresh).await?;
                    Ok::<_, UiError>((fresh, fd))
                })?
            }
        };

        match run_gst_screencast_frame(fd, sess.node_id, out_path) {
            Ok(()) => {
                sess.last_used = Instant::now();
                *screencast_pool().lock().unwrap_or_else(|e| e.into_inner()) = Some(sess);
                ensure_screencast_reaper();
                return Ok(());
            }
            Err(err) if is_reused && attempt == 0 => {
                log::debug!("reopening stale ScreenCast session after error: {err}");
                close_screencast_session(sess);
                continue;
            }
            Err(err) => {
                close_screencast_session(sess);
                return Err(err);
            }
        }
    }

    unreachable!("loop returns within two attempts")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_lsb_bgrx_to_opaque_rgba() {
        // B=0x10, G=0x20, R=0x30, X=0x00 -> RGBA (0x30, 0x20, 0x10, 0xff)
        let raw = [0x10u8, 0x20, 0x30, 0x00, 0xaa, 0xbb, 0xcc, 0x00];
        let img = decode_zpixmap_rgba(&raw, 2, 1, 24, 32, ImageOrder::LSB_FIRST).unwrap();
        assert_eq!(img.get_pixel(0, 0).0, [0x30, 0x20, 0x10, 255]);
        assert_eq!(img.get_pixel(1, 0).0, [0xcc, 0xbb, 0xaa, 255]);
    }

    #[test]
    fn rejects_truncated_zpixmap() {
        let raw = [0u8; 4];
        assert!(decode_zpixmap_rgba(&raw, 2, 1, 24, 32, ImageOrder::LSB_FIRST).is_err());
    }

    #[test]
    fn z_order_maps_same_pid_windows_without_helper_theft() {
        let x11 = vec![
            // Tiny topmost helper window with no matching AT-SPI top-level window.
            X11WindowMeta {
                pid: Some(100),
                title: None,
                rect: (0, 0, 10, 10),
            },
            X11WindowMeta {
                pid: Some(100),
                title: Some("Main".into()),
                rect: (100, 100, 1200, 800),
            },
            X11WindowMeta {
                pid: Some(100),
                title: Some("Secondary".into()),
                rect: (50, 50, 800, 600),
            },
        ];
        let atspi = vec![
            AtspiWindowMeta {
                hwnd: 0xB,
                pid: Some(100),
                title: Some("Secondary".into()),
                bounds: Some((50, 50, 800, 600)),
            },
            AtspiWindowMeta {
                hwnd: 0xA,
                pid: Some(100),
                title: Some("Main".into()),
                bounds: Some((100, 100, 1200, 800)),
            },
        ];
        assert_eq!(match_z_order_to_hwnds(&x11, &atspi), vec![0xA, 0xB]);
    }

    #[test]
    fn z_order_falls_back_to_title_when_pid_missing_only_on_match() {
        let x11 = vec![
            X11WindowMeta {
                pid: None,
                title: Some("Terminal".into()),
                rect: (0, 0, 800, 600),
            },
            X11WindowMeta {
                pid: None,
                title: Some("Unrelated".into()),
                rect: (0, 0, 800, 600),
            },
        ];
        let atspi = vec![
            AtspiWindowMeta {
                hwnd: 0x1,
                pid: Some(200),
                title: Some("Notes".into()),
                bounds: Some((0, 0, 800, 600)),
            },
            AtspiWindowMeta {
                hwnd: 0x2,
                pid: Some(300),
                title: Some("Terminal".into()),
                bounds: Some((10, 10, 800, 600)),
            },
        ];
        assert_eq!(match_z_order_to_hwnds(&x11, &atspi), vec![0x2]);
    }

    #[test]
    fn capture_session_guard_tracks_x11_and_screencast_refcounts() {
        let base_x11 = X11_SESSION_DEPTH.load(Ordering::Acquire);
        let base_sc = SCREENCAST_SESSION_DEPTH.load(Ordering::Acquire);
        {
            let _g1 = enter_capture_session(true, Some(Duration::from_secs(15)));
            assert_eq!(X11_SESSION_DEPTH.load(Ordering::Acquire), base_x11 + 1);
            assert_eq!(SCREENCAST_SESSION_DEPTH.load(Ordering::Acquire), base_sc + 1);
            assert!(x11_keepalive_enabled());
            assert_eq!(screencast_keepalive_timeout(), Some(Duration::from_secs(15)));

            {
                let _g2 = enter_capture_session(true, None);
                assert_eq!(X11_SESSION_DEPTH.load(Ordering::Acquire), base_x11 + 2);
                assert_eq!(SCREENCAST_SESSION_DEPTH.load(Ordering::Acquire), base_sc + 1);
            }
            assert_eq!(X11_SESSION_DEPTH.load(Ordering::Acquire), base_x11 + 1);
        }
        assert_eq!(X11_SESSION_DEPTH.load(Ordering::Acquire), base_x11);
        assert_eq!(SCREENCAST_SESSION_DEPTH.load(Ordering::Acquire), base_sc);
    }

    #[test]
    fn intersect_and_gap_helpers_detect_multimonitor_layouts() {
        assert_eq!(
            intersect_rect((100, 100, 400, 400), (0, 0, 300, 300)),
            Some((100, 100, 200, 200))
        );
        assert_eq!(intersect_rect((500, 0, 100, 100), (0, 0, 300, 300)), None);
        // Two contiguous side-by-side monitors: no gap.
        assert!(!monitors_have_gap(&[(0, 0, 1920, 1080), (1920, 0, 1920, 1080)]));
        // L-shaped asymmetric monitors: bounding box contains dead zone.
        assert!(monitors_have_gap(&[(0, 0, 1920, 1080), (1920, 0, 1280, 720)]));
        // Physically separated monitors with dead column gap.
        assert!(monitors_have_gap(&[(0, 0, 1920, 1080), (2000, 0, 1920, 1080)]));
    }
}
