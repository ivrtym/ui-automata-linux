use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use image::GenericImageView;

use crate::a11y::{A11yClient, synthetic_hwnd};
use crate::backend::{
    OcrBackend, ScreenshotBackend, has_x11_display, is_screenshot_backend_explicit,
    resolve_ocr_backend, resolve_screenshot_backend,
};
use crate::capture;
use crate::{ClickType, Desktop, LinuxElement, Result, UiError, input};

/// A recognized text block with screen-space bounding box.
#[derive(Debug, Clone, serde::Serialize)]
pub struct OcrBlock {
    pub text: String,
    pub confidence: f32,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl OcrBlock {
    pub fn center(&self) -> (i32, i32) {
        (self.x + self.width / 2, self.y + self.height / 2)
    }
}

/// Complete OCR layout for a window, screen region, or image.
#[derive(Debug, Clone, serde::Serialize)]
pub struct OcrLayout {
    pub width: u32,
    pub height: u32,
    pub origin_x: i32,
    pub origin_y: i32,
    pub backend: String,
    pub blocks: Vec<OcrBlock>,
}

impl OcrLayout {
    pub fn full_text(&self) -> String {
        self.blocks
            .iter()
            .map(|b| b.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Find the first OCR block whose text contains `query` (case-insensitive).
    pub fn find_text(&self, query: &str) -> Option<&OcrBlock> {
        let needle = query.to_lowercase();
        self.blocks
            .iter()
            .find(|b| b.text.to_lowercase().contains(&needle))
    }

    /// Find all OCR blocks whose text contains `query` (case-insensitive).
    pub fn find_all_text(&self, query: &str) -> Vec<&OcrBlock> {
        let needle = query.to_lowercase();
        self.blocks
            .iter()
            .filter(|b| b.text.to_lowercase().contains(&needle))
            .collect()
    }
}

fn ensure_parent_dir(out_path: &Path) -> Result<()> {
    if let Some(parent) = out_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    Ok(())
}

fn save_dynamic_image(img: &image::DynamicImage, out_path: &Path) -> Result<()> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

    ensure_parent_dir(out_path)?;
    let parent = out_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp_path = parent.join(format!(".tmp-{}-{seq}.png", std::process::id()));

    let format = image::ImageFormat::from_path(out_path).unwrap_or(image::ImageFormat::Png);
    let write_res: std::result::Result<(), String> = (|| {
        #[cfg(unix)]
        let mut file = {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&tmp_path)
                .map_err(|e| e.to_string())?
        };
        #[cfg(not(unix))]
        let mut file = std::fs::File::create(&tmp_path).map_err(|e| e.to_string())?;

        img.write_to(&mut file, format).map_err(|e| e.to_string())?;
        file.flush().map_err(|e| e.to_string())?;
        drop(file);
        std::fs::rename(&tmp_path, out_path).map_err(|e| e.to_string())?;
        Ok(())
    })();

    if let Err(err) = write_res {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(UiError::Platform(format!(
            "failed to atomically save screenshot {}: {err}",
            out_path.display()
        ))
        .into());
    }
    Ok(())
}

fn crop_screen_image(
    img: &image::DynamicImage,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
) -> Result<image::DynamicImage> {
    let cx = x.max(0) as u32;
    let cy = y.max(0) as u32;
    let cw = (width as u32).min(img.width().saturating_sub(cx));
    let ch = (height as u32).min(img.height().saturating_sub(cy));
    if cw == 0 || ch == 0 {
        return Err(UiError::Internal("cropped region is empty".into()).into());
    }
    Ok(img.crop_imm(cx, cy, cw, ch))
}

fn resolve_window_element(hwnd: u64) -> Result<LinuxElement> {
    let desktop = Desktop::new();
    desktop
        .application_windows()?
        .into_iter()
        .find(|w| synthetic_hwnd(w.bus_name(), w.object_path()) == hwnd)
        .ok_or_else(|| UiError::Internal(format!("window hwnd={hwnd:#x} not found")).into())
}

/// Capture the entire screen to `out_path` (PNG).
pub fn capture_screen(out_path: &Path) -> Result<()> {
    ensure_parent_dir(out_path)?;
    let backend = resolve_screenshot_backend()?;
    match backend {
        ScreenshotBackend::X11rb => {
            let img = capture::capture_x11rb_screen()?;
            save_dynamic_image(&img, out_path)
        }
        ScreenshotBackend::ScreenCast => capture::capture_via_screencast(out_path),
        ScreenshotBackend::Grim => run_capture_cmd("grim", &[out_path.to_str().unwrap_or("")]),
        ScreenshotBackend::GnomeScreenshot => {
            run_capture_cmd("gnome-screenshot", &["-f", out_path.to_str().unwrap_or("")])
        }
        ScreenshotBackend::Import => {
            run_capture_cmd("import", &["-window", "root", out_path.to_str().unwrap_or("")])
        }
        ScreenshotBackend::Scrot => {
            run_capture_cmd("scrot", &["-o", out_path.to_str().unwrap_or("")])
        }
        ScreenshotBackend::Portal => capture_via_portal(out_path),
    }
}

/// Capture a screen rectangle `(x, y, width, height)` to `out_path` (PNG).
pub fn capture_region(x: i32, y: i32, width: i32, height: i32, out_path: &Path) -> Result<()> {
    if width <= 0 || height <= 0 {
        return Err(UiError::Internal(format!(
            "capture_region requires positive dimensions, got {width}x{height}"
        ))
        .into());
    }
    ensure_parent_dir(out_path)?;
    let backend = resolve_screenshot_backend()?;
    match backend {
        ScreenshotBackend::X11rb => {
            let img = capture::capture_x11rb_region(x, y, width, height)?;
            save_dynamic_image(&img, out_path)
        }
        ScreenshotBackend::Grim => {
            let geom = format!("{x},{y} {width}x{height}");
            run_capture_cmd("grim", &["-g", &geom, out_path.to_str().unwrap_or("")])
        }
        ScreenshotBackend::Import => {
            let geom = format!("{width}x{height}+{x}+{y}");
            run_capture_cmd(
                "import",
                &["-window", "root", "-crop", &geom, out_path.to_str().unwrap_or("")],
            )
        }
        ScreenshotBackend::Scrot => {
            let area = format!("{x},{y},{width},{height}");
            run_capture_cmd("scrot", &["-o", "-a", &area, out_path.to_str().unwrap_or("")])
        }
        ScreenshotBackend::ScreenCast
        | ScreenshotBackend::GnomeScreenshot
        | ScreenshotBackend::Portal => {
            let tmp = temp_png_path("full");
            capture_screen(&tmp)?;
            let img = image::open(&tmp);
            let _ = std::fs::remove_file(&tmp);
            let cropped = crop_screen_image(&img?, x, y, width, height)?;
            save_dynamic_image(&cropped, out_path)
        }
    }
}

/// Capture a window-scoped image `(DynamicImage, origin_x, origin_y)` into memory when possible.
///
/// In `auto` mode on X11 or XWayland, resolves the target window's XID via `x11rb` so
/// window/region captures complete in-memory with zero full-screen flash or Portal overhead,
/// falling back to the session screenshot backend for native Wayland clients.
pub(crate) fn capture_target_image(
    hwnd: Option<u64>,
    region: Option<(i32, i32, i32, i32)>,
) -> Result<(image::DynamicImage, i32, i32)> {
    let backend = resolve_screenshot_backend()?;
    let explicit = is_screenshot_backend_explicit();

    if let Some(h) = hwnd {
        let win = resolve_window_element(h)?;
        let try_x11rb = backend == ScreenshotBackend::X11rb || (!explicit && has_x11_display());
        if try_x11rb {
            let pid = win.process_id().ok();
            let title = win.name();
            let bounds = win.bounds().ok();
            match capture::capture_x11rb_window(pid, title.as_deref(), bounds, region) {
                Ok(Some(res)) => return Ok(res),
                Ok(None) if backend == ScreenshotBackend::X11rb => {
                    return Err(UiError::Platform(format!(
                        "screenshot backend 'x11rb' found no matching X11 window for hwnd={h:#x}"
                    ))
                    .into());
                }
                Err(e) if backend == ScreenshotBackend::X11rb => return Err(e),
                Ok(None) | Err(_) => {
                    // Pure Wayland client or transient X11 mismatch in auto mode: fall back below.
                }
            }
        }

        if let Some((rx, ry, rw, rh)) = region {
            let tmp = temp_png_path("win_reg");
            capture_region(rx, ry, rw, rh, &tmp)?;
            let img = image::open(&tmp);
            let _ = std::fs::remove_file(&tmp);
            return Ok((img?, rx, ry));
        }

        let (x, y, w, h_px) = win.bounds().unwrap_or((0, 0, 0, 0));
        let wayland_unknown_origin = matches!(
            crate::backend::resolve_display_backend().ok(),
            Some(crate::backend::DisplayBackend::Wayland)
        ) && x == 0
            && y == 0;
        let tmp = temp_png_path("win");
        let (ox, oy) = if w > 0 && h_px > 0 && !wayland_unknown_origin {
            capture_region(x, y, w, h_px, &tmp)?;
            (x, y)
        } else {
            capture_screen(&tmp)?;
            (0, 0)
        };
        let img = image::open(&tmp);
        let _ = std::fs::remove_file(&tmp);
        return Ok((img?, ox, oy));
    }

    if let Some((rx, ry, rw, rh)) = region {
        if backend == ScreenshotBackend::X11rb {
            let img = capture::capture_x11rb_region(rx, ry, rw, rh)?;
            return Ok((img, rx, ry));
        }
        let tmp = temp_png_path("reg");
        capture_region(rx, ry, rw, rh, &tmp)?;
        let img = image::open(&tmp);
        let _ = std::fs::remove_file(&tmp);
        return Ok((img?, rx, ry));
    }

    if backend == ScreenshotBackend::X11rb {
        let img = capture::capture_x11rb_screen()?;
        return Ok((img, 0, 0));
    }
    let tmp = temp_png_path("scr");
    capture_screen(&tmp)?;
    let img = image::open(&tmp);
    let _ = std::fs::remove_file(&tmp);
    Ok((img?, 0, 0))
}

/// Capture a window or screen-space sub-region of a window to `out_path` (PNG).
///
/// Returns `(origin_x, origin_y)` of the captured image in screen coordinates.
pub fn capture_window_region(
    hwnd: u64,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    out_path: &Path,
) -> Result<(i32, i32)> {
    if width <= 0 || height <= 0 {
        return Err(UiError::Internal(format!(
            "capture_window_region requires positive dimensions, got {width}x{height}"
        ))
        .into());
    }
    let (img, ox, oy) = capture_target_image(Some(hwnd), Some((x, y, width, height)))?;
    save_dynamic_image(&img, out_path)?;
    Ok((ox, oy))
}

/// Capture a top-level window identified by its synthetic `hwnd`.
///
/// Returns `(origin_x, origin_y)` of the captured image in screen coordinates.
pub fn capture_window(hwnd: u64, out_path: &Path) -> Result<(i32, i32)> {
    let (img, ox, oy) = capture_target_image(Some(hwnd), None)?;
    save_dynamic_image(&img, out_path)?;
    Ok((ox, oy))
}

/// Run OCR directly on an in-memory `DynamicImage` (skipping PNG disk I/O for PP-OCR).
pub(crate) fn recognize_dynamic_image_with_origin(
    img: &image::DynamicImage,
    origin_x: i32,
    origin_y: i32,
    tesseract_path: Option<&Path>,
) -> Result<OcrLayout> {
    let (width, height) = img.dimensions();
    let backend = resolve_ocr_backend()?;

    let mut blocks = match backend {
        OcrBackend::OrtPpOcr => {
            #[cfg(feature = "ocr-ort")]
            {
                crate::vision_ort::recognize_with_ort(img)?
            }
            #[cfg(not(feature = "ocr-ort"))]
            {
                return Err(UiError::Platform(
                    "ocr-ort feature is disabled at compile time".into(),
                )
                .into());
            }
        }
        OcrBackend::Tesseract => match tesseract_path {
            Some(p) => recognize_with_tesseract(p)?,
            None => {
                let tmp = temp_png_path("tess");
                save_dynamic_image(img, &tmp)?;
                let res = recognize_with_tesseract(&tmp);
                let _ = std::fs::remove_file(&tmp);
                res?
            }
        },
        OcrBackend::OmniParser => crate::vision_omniparser::recognize_with_omniparser(img)?,
    };

    if origin_x != 0 || origin_y != 0 {
        for b in &mut blocks {
            b.x += origin_x;
            b.y += origin_y;
        }
    }

    Ok(OcrLayout {
        width,
        height,
        origin_x,
        origin_y,
        backend: backend.as_str().to_string(),
        blocks,
    })
}

/// Capture the optional window/region target and run OCR (in-memory for `x11rb` + `ort-ppocr`).
pub(crate) fn recognize_target(
    hwnd: Option<u64>,
    region: Option<(i32, i32, i32, i32)>,
) -> Result<OcrLayout> {
    let (img, ox, oy) = capture_target_image(hwnd, region)?;
    recognize_dynamic_image_with_origin(&img, ox, oy, None)
}

/// Run OCR on an image file on disk.
pub fn recognize_image(path: &Path) -> Result<OcrLayout> {
    recognize_image_with_origin(path, 0, 0)
}

/// Run OCR on an image file on disk and offset all bounding boxes by `(origin_x, origin_y)`.
pub fn recognize_image_with_origin(path: &Path, origin_x: i32, origin_y: i32) -> Result<OcrLayout> {
    let img = image::open(path).map_err(|e| {
        UiError::Platform(format!("failed to open image {}: {e}", path.display()))
    })?;
    recognize_dynamic_image_with_origin(&img, origin_x, origin_y, Some(path))
}

/// Capture the entire screen and run OCR (mirrors `vision screen_layout`).
pub fn screen_layout() -> Result<OcrLayout> {
    recognize_target(None, None)
}

/// Extended vision layout response with optional saved screenshot path (for MCP `vision`).
#[derive(Debug, Clone, serde::Serialize)]
pub struct VisionLayoutOutput {
    pub width: u32,
    pub height: u32,
    pub origin_x: i32,
    pub origin_y: i32,
    pub backend: String,
    pub blocks: Vec<OcrBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot_path: Option<String>,
}

/// Discovered interactive region from [`window_probe`].
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProbeCandidate {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub kind: String,
    pub label: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct WindowProbeOutput {
    pub layout: VisionLayoutOutput,
    pub candidates: Vec<ProbeCandidate>,
}

fn save_downloads_screenshot(img: &image::DynamicImage) -> Result<String> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let dir = PathBuf::from(home).join("Downloads");
    let _ = std::fs::create_dir_all(&dir);
    let ts = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let webp_path = dir.join(format!("Screenshot {ts}.webp"));
    if img.save(&webp_path).is_ok() {
        return Ok(webp_path.display().to_string());
    }
    let png_path = dir.join(format!("Screenshot {ts}.png"));
    save_dynamic_image(img, &png_path)?;
    Ok(png_path.display().to_string())
}

/// Capture window (`Some(hwnd)`) or full screen (`None`) with optional OCR skip and file export.
pub fn vision_layout_ex(
    hwnd: Option<u64>,
    capture_only: bool,
    save_screenshot: bool,
) -> Result<VisionLayoutOutput> {
    let (img, ox, oy) = capture_target_image(hwnd, None)?;
    let screenshot_path = if save_screenshot {
        Some(save_downloads_screenshot(&img)?)
    } else {
        None
    };

    if capture_only {
        let (width, height) = img.dimensions();
        return Ok(VisionLayoutOutput {
            width,
            height,
            origin_x: ox,
            origin_y: oy,
            backend: "capture-only".into(),
            blocks: Vec::new(),
            screenshot_path,
        });
    }

    let layout = recognize_dynamic_image_with_origin(&img, ox, oy, None)?;
    Ok(VisionLayoutOutput {
        width: layout.width,
        height: layout.height,
        origin_x: layout.origin_x,
        origin_y: layout.origin_y,
        backend: layout.backend,
        blocks: layout.blocks,
        screenshot_path,
    })
}

/// Discover non-AT-SPI2 interactive hotspots in `hwnd` on top of OCR `window_layout`
/// by sampling uncovered grid points with pointer hover and detecting visual/tooltip diffs.
pub fn window_probe(hwnd: u64, capture_only: bool, save_screenshot: bool) -> Result<WindowProbeOutput> {
    let layout = vision_layout_ex(Some(hwnd), capture_only, save_screenshot)?;
    let (base_img, ox, oy) = capture_target_image(Some(hwnd), None)?;
    let (w, h) = base_img.dimensions();
    let mut candidates = Vec::new();

    if w >= 60 && h >= 60 {
        let base_rgb = base_img.to_rgb8();
        let cols = 4i32;
        let rows = 4i32;
        let step_x = (w as i32) / (cols + 1);
        let step_y = (h as i32) / (rows + 1);

        'probe: for r in 1..=rows {
            for c in 1..=cols {
                if candidates.len() >= 12 {
                    break 'probe;
                }
                let lx = c * step_x;
                let ly = r * step_y;
                let sx = ox + lx;
                let sy = oy + ly;
                if layout
                    .blocks
                    .iter()
                    .any(|b| sx >= b.x && sx <= b.x + b.width && sy >= b.y && sy <= b.y + b.height)
                {
                    continue;
                }
                if input::mouse_move(sx, sy).is_err() {
                    continue;
                }
                std::thread::sleep(std::time::Duration::from_millis(80));
                if let Ok((after_img, _, _)) = capture_target_image(Some(hwnd), None) {
                    let after_rgb = after_img.to_rgb8();
                    if after_rgb.dimensions() == base_rgb.dimensions() {
                        let changed = base_rgb
                            .pixels()
                            .zip(after_rgb.pixels())
                            .step_by(8)
                            .filter(|(p1, p2)| {
                                (p1[0] as i16 - p2[0] as i16).abs()
                                    + (p1[1] as i16 - p2[1] as i16).abs()
                                    + (p1[2] as i16 - p2[2] as i16).abs()
                                    > 36
                            })
                            .count();
                        if changed >= 12 {
                            candidates.push(ProbeCandidate {
                                x: sx - 20,
                                y: sy - 14,
                                width: 40,
                                height: 28,
                                kind: "hover_reactive".into(),
                                label: format!("reactive@{sx},{sy}"),
                            });
                        }
                    }
                }
            }
        }
    }

    Ok(WindowProbeOutput { layout, candidates })
}

/// Capture a specific window and run OCR (mirrors `vision window_layout`).
pub fn window_layout(hwnd: u64) -> Result<OcrLayout> {
    recognize_target(Some(hwnd), None)
}

/// OCR fallback click: recognize text in `hwnd` (or full screen when `None`),
/// find the first block containing `query`, and click its center.
pub fn click_text(hwnd: Option<u64>, query: &str, kind: ClickType) -> Result<OcrBlock> {
    let layout = match hwnd {
        Some(h) => window_layout(h)?,
        None => screen_layout()?,
    };
    let found = layout
        .find_text(query)
        .cloned()
        .ok_or_else(|| {
            UiError::Internal(format!(
                "OCR text '{query}' not found (backend={}, blocks={})",
                layout.backend,
                layout.blocks.len()
            ))
        })?;
    let (cx, cy) = found.center();
    input::mouse_click(cx, cy, kind)?;
    Ok(found)
}

fn recognize_with_tesseract(path: &Path) -> Result<Vec<OcrBlock>> {
    let lang = std::env::var("AUTOMATA_OCR_LANG")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "chi_sim+eng".to_string());

    let out = Command::new("tesseract")
        .args([
            path.to_str().unwrap_or(""),
            "stdout",
            "-l",
            &lang,
            "--psm",
            "6",
            "tsv",
        ])
        .output()
        .map_err(|e| UiError::Platform(format!("failed to spawn tesseract: {e}")))?;

    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(UiError::Platform(format!(
            "tesseract failed ({}): {}",
            out.status,
            err.trim()
        ))
        .into());
    }

    let tsv = String::from_utf8_lossy(&out.stdout);
    Ok(parse_tesseract_tsv(&tsv))
}

/// Parse `tesseract ... tsv` output and merge words belonging to the same line
/// `(page_num, block_num, par_num, line_num)` into clean line-level `OcrBlock`s.
pub fn parse_tesseract_tsv(tsv: &str) -> Vec<OcrBlock> {
    type LineKey = (i32, i32, i32, i32);
    struct LineAccum {
        words: Vec<String>,
        conf_sum: f32,
        conf_n: u32,
        min_x: i32,
        min_y: i32,
        max_x: i32,
        max_y: i32,
    }

    let mut lines: BTreeMap<LineKey, LineAccum> = BTreeMap::new();

    for (idx, row) in tsv.lines().enumerate() {
        if idx == 0 && row.starts_with("level\t") {
            continue;
        }
        let cols: Vec<&str> = row.split('\t').collect();
        if cols.len() < 12 {
            continue;
        }
        let level: i32 = cols[0].parse().unwrap_or(0);
        if level != 5 {
            continue;
        }
        let text = cols[11].trim();
        if text.is_empty() {
            continue;
        }
        let conf: f32 = cols[10].parse().unwrap_or(-1.0);
        if conf < 0.0 {
            continue;
        }

        let page: i32 = cols[1].parse().unwrap_or(0);
        let block: i32 = cols[2].parse().unwrap_or(0);
        let par: i32 = cols[3].parse().unwrap_or(0);
        let line: i32 = cols[4].parse().unwrap_or(0);

        let left: i32 = cols[6].parse().unwrap_or(0);
        let top: i32 = cols[7].parse().unwrap_or(0);
        let width: i32 = cols[8].parse().unwrap_or(0);
        let height: i32 = cols[9].parse().unwrap_or(0);
        if width <= 0 || height <= 0 {
            continue;
        }

        let key = (page, block, par, line);
        let entry = lines.entry(key).or_insert_with(|| LineAccum {
            words: Vec::new(),
            conf_sum: 0.0,
            conf_n: 0,
            min_x: left,
            min_y: top,
            max_x: left + width,
            max_y: top + height,
        });
        entry.words.push(text.to_string());
        entry.conf_sum += (conf / 100.0).clamp(0.0, 1.0);
        entry.conf_n += 1;
        entry.min_x = entry.min_x.min(left);
        entry.min_y = entry.min_y.min(top);
        entry.max_x = entry.max_x.max(left + width);
        entry.max_y = entry.max_y.max(top + height);
    }

    lines
        .into_values()
        .map(|acc| OcrBlock {
            text: join_ocr_line_words(&acc.words),
            confidence: if acc.conf_n > 0 {
                acc.conf_sum / acc.conf_n as f32
            } else {
                0.0
            },
            x: acc.min_x,
            y: acc.min_y,
            width: (acc.max_x - acc.min_x).max(0),
            height: (acc.max_y - acc.min_y).max(0),
        })
        .collect()
}

fn is_cjk_char(ch: char) -> bool {
    matches!(
        ch as u32,
        0x3000..=0x303F // CJK Symbols and Punctuation
        | 0x3040..=0x30FF // Hiragana & Katakana
        | 0x3400..=0x4DBF // CJK Unified Ideographs Extension A
        | 0x4E00..=0x9FFF // CJK Unified Ideographs
        | 0xF900..=0xFAFF // CJK Compatibility Ideographs
        | 0xFF00..=0xFFEF // Halfwidth and Fullwidth Forms
    )
}

fn join_ocr_line_words(words: &[String]) -> String {
    let mut out = String::new();
    for word in words {
        if word.is_empty() {
            continue;
        }
        if let (Some(prev_last), Some(next_first)) = (out.chars().next_back(), word.chars().next()) {
            if !(is_cjk_char(prev_last) && is_cjk_char(next_first)) {
                out.push(' ');
            }
        }
        out.push_str(word);
    }
    out
}

fn run_capture_cmd(bin: &str, args: &[&str]) -> Result<()> {
    let out = Command::new(bin)
        .args(args)
        .output()
        .map_err(|e| UiError::Platform(format!("failed to spawn {bin}: {e}")))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(UiError::Platform(format!(
            "{bin} {:?} failed ({}): {}",
            args,
            out.status,
            err.trim()
        ))
        .into());
    }
    Ok(())
}

fn capture_via_portal(out_path: &Path) -> Result<()> {
    use std::collections::HashMap;
    use zbus::zvariant::{OwnedObjectPath, Value};

    let client = A11yClient::shared()?;
    let uri: String = client.block_on(async {
        let conn = zbus::Connection::session()
            .await
            .map_err(|e| UiError::Platform(format!("session dbus failed: {e}")))?;
        let proxy = zbus::Proxy::new(
            &conn,
            "org.freedesktop.portal.Desktop",
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.Screenshot",
        )
        .await
        .map_err(|e| UiError::Platform(format!("Screenshot portal proxy failed: {e}")))?;

        let token = format!("automata_{}", std::process::id());
        let mut opts: HashMap<&str, Value<'_>> = HashMap::new();
        opts.insert("handle_token", Value::from(token.as_str()));
        opts.insert("interactive", Value::from(false));

        let req_path: OwnedObjectPath = proxy
            .call("Screenshot", &("", opts))
            .await
            .map_err(|e| UiError::Platform(format!("Portal Screenshot call failed: {e}")))?;

        let req_proxy = zbus::Proxy::new(
            &conn,
            "org.freedesktop.portal.Desktop",
            req_path.as_str(),
            "org.freedesktop.portal.Request",
        )
        .await
        .map_err(|e| UiError::Platform(format!("Portal Request proxy failed: {e}")))?;

        use futures_util::StreamExt;
        let mut stream = req_proxy
            .receive_signal("Response")
            .await
            .map_err(|e| UiError::Platform(format!("Portal Response listen failed: {e}")))?;

        let msg = tokio::time::timeout(std::time::Duration::from_secs(10), stream.next())
            .await
            .map_err(|_| UiError::Platform("Portal Screenshot timed out".into()))?
            .ok_or_else(|| UiError::Platform("Portal Screenshot stream ended".into()))?;

        let (code, results): (u32, HashMap<String, zbus::zvariant::OwnedValue>) = msg
            .body()
            .deserialize()
            .map_err(|e| UiError::Platform(format!("Portal Response decode failed: {e}")))?;

        if code != 0 {
            return Err(UiError::Platform(format!(
                "Portal Screenshot cancelled/denied (response={code})"
            )));
        }

        let uri_val = results
            .get("uri")
            .ok_or_else(|| UiError::Platform("Portal Screenshot returned no uri".into()))?;
        let uri: String = uri_val
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .map_err(|_| UiError::Platform("Portal Screenshot uri was not a string".into()))?;
        Ok::<String, UiError>(uri)
    })?;

    if let Some(parent) = out_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let src = decode_file_uri(&uri);
    std::fs::copy(&src, out_path).map_err(|e| {
        UiError::Platform(format!(
            "failed to copy portal screenshot '{src}' -> '{}': {e}",
            out_path.display()
        ))
    })?;
    let _ = std::fs::remove_file(&src);
    Ok(())
}

/// Decode a `file://` URI (including UTF-8 percent-escapes such as `%E5%9B%BE%E7%89%87`) into a local path string.
fn decode_file_uri(uri: &str) -> String {
    let raw = uri.strip_prefix("file://").unwrap_or(uri);
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn temp_png_path(prefix: &str) -> PathBuf {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("ui_automata_{prefix}_{}_{ts}.png", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::{decode_file_uri, parse_tesseract_tsv};

    #[test]
    fn decodes_percent_encoded_file_uri() {
        let uri = "file:///home/ivrty/%E5%9B%BE%E7%89%87/Screenshot%2013.png";
        assert_eq!(decode_file_uri(uri), "/home/ivrty/图片/Screenshot 13.png");
    }

    #[test]
    fn parses_and_merges_tesseract_tsv_lines() {
        let sample = "\
level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext
1\t1\t0\t0\t0\t0\t0\t0\t800\t600\t-1\t
5\t1\t1\t1\t1\t1\t100\t50\t40\t20\t95\tHello
5\t1\t1\t1\t1\t2\t150\t50\t50\t20\t91\tWorld
5\t1\t1\t1\t2\t1\t100\t90\t60\t22\t88\tSubmit";
        let blocks = parse_tesseract_tsv(sample);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].text, "Hello World");
        assert_eq!((blocks[0].x, blocks[0].y, blocks[0].width, blocks[0].height), (100, 50, 100, 20));
        assert_eq!(blocks[0].center(), (150, 60));
        assert_eq!(blocks[1].text, "Submit");
    }

    #[test]
    fn merges_cjk_tesseract_tokens_without_spaces() {
        let sample = "\
level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext
5\t1\t1\t1\t1\t1\t10\t20\t12\t14\t92\t明
5\t1\t1\t1\t1\t2\t23\t20\t12\t14\t91\t珠
5\t1\t1\t1\t1\t3\t36\t20\t12\t14\t90\t囚
5\t1\t1\t1\t1\t4\t49\t20\t12\t14\t89\t瑕
5\t1\t1\t1\t2\t1\t10\t40\t10\t14\t95\tY
5\t1\t1\t1\t2\t2\t24\t40\t24\t14\t94\t联系
5\t1\t1\t1\t2\t3\t49\t40\t12\t14\t93\t人
5\t1\t1\t1\t2\t4\t66\t40\t10\t14\t90\t5";
        let blocks = parse_tesseract_tsv(sample);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].text, "明珠囚瑕");
        assert_eq!(blocks[1].text, "Y 联系人 5");
    }
}
