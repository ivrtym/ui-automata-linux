use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use image::DynamicImage;
use image::imageops::FilterType;
use ndarray::Array4;
use ort::environment::Environment;
use ort::session::Session;
use ort::value::TensorRef;

use crate::backend::{
    find_ocr_model_dir, find_ort_dylib, resolve_ppocr_file, resolve_ppocr_keys,
};
use crate::vision::OcrBlock;
use crate::{Result, UiError};

struct PpOcrSessions {
    det: Session,
    rec: Session,
    cls: Option<Session>,
    keys: Vec<String>,
}

static ORT_ENV: OnceLock<std::result::Result<Environment, String>> = OnceLock::new();
static PPOCR_ENGINE: OnceLock<std::result::Result<Mutex<PpOcrSessions>, String>> = OnceLock::new();

fn get_env() -> Result<&'static Environment> {
    let res = ORT_ENV.get_or_init(|| {
        let dylib = find_ort_dylib().ok_or_else(|| {
            "libonnxruntime.so not found; set ORT_DYLIB_PATH or install libonnxruntime".to_string()
        })?;
        let builder = ort::init_from(&dylib)
            .map_err(|e| format!("ort::init_from({}) failed: {e}", dylib.display()))?;
        builder
            .build()
            .map_err(|e| format!("ort environment build failed: {e}"))
    });
    match res {
        Ok(env) => Ok(env),
        Err(e) => Err(UiError::Platform(e.clone()).into()),
    }
}

fn get_sessions() -> Result<&'static Mutex<PpOcrSessions>> {
    let res = PPOCR_ENGINE.get_or_init(|| {
        let model_dir = find_ocr_model_dir().ok_or_else(|| {
            "PP-OCR ONNX model directory not found; set AUTOMATA_OCR_MODEL_DIR".to_string()
        })?;
        load_sessions_from_dir(&model_dir).map(Mutex::new).map_err(|e| e.to_string())
    });
    match res {
        Ok(m) => Ok(m),
        Err(e) => Err(UiError::Platform(e.clone()).into()),
    }
}

fn load_sessions_from_dir(dir: &Path) -> Result<PpOcrSessions> {
    let env = get_env()?;

    let det_path = resolve_ppocr_file(
        dir,
        &["det.onnx", "ch_PP-OCRv5_mobile_det.onnx", "ch_PP-OCRv4_det_infer.onnx"],
        "det",
    )
    .ok_or_else(|| UiError::Platform(format!("no det*.onnx found in {}", dir.display())))?;

    let rec_path = resolve_ppocr_file(
        dir,
        &["rec.onnx", "ch_PP-OCRv5_mobile_rec.onnx", "ch_PP-OCRv4_rec_infer.onnx"],
        "rec",
    )
    .ok_or_else(|| UiError::Platform(format!("no rec*.onnx found in {}", dir.display())))?;

    let keys_path = resolve_ppocr_keys(dir)
        .ok_or_else(|| UiError::Platform(format!("no keys.txt / dict.txt found in {}", dir.display())))?;

    let det = Session::builder(env)
        .map_err(|e| UiError::Platform(e.to_string()))?
        .commit_from_file(&det_path)
        .map_err(|e| UiError::Platform(format!("load det model {}: {e}", det_path.display())))?;

    let rec = Session::builder(env)
        .map_err(|e| UiError::Platform(e.to_string()))?
        .commit_from_file(&rec_path)
        .map_err(|e| UiError::Platform(format!("load rec model {}: {e}", rec_path.display())))?;

    let cls = if let Some(cls_path) =
        resolve_ppocr_file(dir, &["cls.onnx", "ch_ppocr_mobile_v2.0_cls_infer.onnx"], "cls")
    {
        Session::builder(env)
            .ok()
            .and_then(|mut b| b.commit_from_file(&cls_path).ok())
    } else {
        None
    };

    let keys_raw = std::fs::read_to_string(&keys_path)
        .map_err(|e| UiError::Platform(format!("read keys {}: {e}", keys_path.display())))?;
    // Index 0 = CTC blank; 1..=N = lines from dictionary; N+1 = space (PP-OCR convention).
    let mut keys = Vec::with_capacity(7000);
    keys.push(String::new());
    for line in keys_raw.lines() {
        keys.push(line.trim_end_matches('\r').to_string());
    }
    keys.push(" ".to_string());

    Ok(PpOcrSessions {
        det,
        rec,
        cls,
        keys,
    })
}

/// Run PP-OCRv4/v5 Mobile ONNX (det -> optional cls -> rec) on an in-memory image.
pub fn recognize_with_ort(img: &DynamicImage) -> Result<Vec<OcrBlock>> {
    let mutex = get_sessions()?;
    let mut sessions = mutex.lock().unwrap();

    let orig_w = img.width().max(1);
    let orig_h = img.height().max(1);

    // 1. Detection preprocess: resize max side <= 960 and round dimensions up to multiples of 32.
    let max_side = 960.0f32;
    let ratio = if (orig_w.max(orig_h) as f32) > max_side {
        max_side / (orig_w.max(orig_h) as f32)
    } else {
        1.0
    };
    let resize_w = ((orig_w as f32 * ratio).round() as u32).max(32);
    let resize_h = ((orig_h as f32 * ratio).round() as u32).max(32);
    let pad_w = resize_w.div_ceil(32) * 32;
    let pad_h = resize_h.div_ceil(32) * 32;

    let resized = img.resize_exact(resize_w, resize_h, FilterType::Triangle).to_rgb8();
    let mut det_tensor = Array4::<f32>::zeros((1, 3, pad_h as usize, pad_w as usize));
    let mean = [0.485f32, 0.456f32, 0.406f32];
    let std = [0.229f32, 0.224f32, 0.225f32];

    for y in 0..resize_h as usize {
        for x in 0..resize_w as usize {
            let p = resized.get_pixel(x as u32, y as u32);
            for c in 0..3 {
                det_tensor[[0, c, y, x]] = (p[c] as f32 / 255.0 - mean[c]) / std[c];
            }
        }
    }

    let PpOcrSessions {
        ref mut det,
        ref mut rec,
        ref mut cls,
        ref keys,
    } = *sessions;

    let det_outputs = det
        .run(ort::inputs![TensorRef::from_array_view(&det_tensor)
            .map_err(|e| UiError::Platform(e.to_string()))?])
        .map_err(|e| UiError::Platform(format!("det inference failed: {e}")))?;
    let (det_shape, det_data) = det_outputs[0]
        .try_extract_tensor::<f32>()
        .map_err(|e| UiError::Platform(format!("det output extract failed: {e}")))?;

    let out_h = det_shape.get(2).copied().unwrap_or(pad_h as i64) as usize;
    let out_w = det_shape.get(3).copied().unwrap_or(pad_w as i64) as usize;

    let boxes = extract_text_boxes(
        det_data,
        out_w,
        out_h,
        resize_w as usize,
        resize_h as usize,
        orig_w,
        orig_h,
    );
    drop(det_outputs);

    let mut results = Vec::new();
    for (bx, by, bw, bh, _det_score) in boxes {
        let mut crop = img.crop_imm(bx, by, bw, bh);

        // 2. Optional 180-degree orientation classification.
        if let Some(cls_sess) = cls.as_mut() {
            if let Ok(rotate_180) = run_cls(cls_sess, &crop) {
                if rotate_180 {
                    crop = crop.rotate180();
                }
            }
        }

        // 3. Recognition (height=48 for PP-OCRv4/v5).
        let (text, rec_conf) = run_rec(rec, keys, &crop)?;
        let trimmed = text.trim();
        if !trimmed.is_empty() && rec_conf >= 0.3 {
            results.push(OcrBlock {
                text: trimmed.to_string(),
                confidence: rec_conf,
                x: bx as i32,
                y: by as i32,
                width: bw as i32,
                height: bh as i32,
            });
        }
    }

    Ok(results)
}

fn extract_text_boxes(
    prob_map: &[f32],
    map_w: usize,
    map_h: usize,
    valid_w: usize,
    valid_h: usize,
    orig_w: u32,
    orig_h: u32,
) -> Vec<(u32, u32, u32, u32, f32)> {
    let thresh = 0.3f32;
    let vw = valid_w.min(map_w);
    let vh = valid_h.min(map_h);
    let mut visited = vec![false; vw * vh];
    let mut raw_boxes: Vec<(u32, u32, u32, u32, f32)> = Vec::new();

    let scale_x = orig_w as f32 / valid_w.max(1) as f32;
    let scale_y = orig_h as f32 / valid_h.max(1) as f32;

    for sy in 0..vh {
        for sx in 0..vw {
            let idx = sy * vw + sx;
            if visited[idx] {
                continue;
            }
            let p = prob_map[sy * map_w + sx];
            if p < thresh {
                continue;
            }

            let mut queue = VecDeque::new();
            queue.push_back((sx, sy));
            visited[idx] = true;

            let (mut min_x, mut max_x) = (sx, sx);
            let (mut min_y, mut max_y) = (sy, sy);
            let mut sum_score = 0.0f32;
            let mut count = 0u32;

            while let Some((cx, cy)) = queue.pop_front() {
                sum_score += prob_map[cy * map_w + cx];
                count += 1;
                min_x = min_x.min(cx);
                max_x = max_x.max(cx);
                min_y = min_y.min(cy);
                max_y = max_y.max(cy);

                for dy in -1isize..=1 {
                    for dx in -1isize..=1 {
                        if dx == 0 && dy == 0 {
                            continue;
                        }
                        let nx = cx as isize + dx;
                        let ny = cy as isize + dy;
                        if nx >= 0 && ny >= 0 && (nx as usize) < vw && (ny as usize) < vh {
                            let (nxu, nyu) = (nx as usize, ny as usize);
                            let nidx = nyu * vw + nxu;
                            if !visited[nidx] && prob_map[nyu * map_w + nxu] >= thresh {
                                visited[nidx] = true;
                                queue.push_back((nxu, nyu));
                            }
                        }
                    }
                }
            }

            let bw = max_x - min_x + 1;
            let bh = max_y - min_y + 1;
            if bw < 4 || bh < 4 || count < 8 {
                continue;
            }
            let mean_score = sum_score / count as f32;
            if mean_score < 0.5 {
                continue;
            }

            // Unclip-style expansion to compensate for DBNet shrunk polygons.
            let pad = ((bw.min(bh) as f32) * 0.35).round() as isize;
            let x0 = ((min_x as isize - pad).max(0) as f32 * scale_x).floor() as u32;
            let y0 = ((min_y as isize - pad).max(0) as f32 * scale_y).floor() as u32;
            let x1 = (((max_x as isize + 1 + pad).min(vw as isize)) as f32 * scale_x)
                .ceil()
                .min(orig_w as f32) as u32;
            let y1 = (((max_y as isize + 1 + pad).min(vh as isize)) as f32 * scale_y)
                .ceil()
                .min(orig_h as f32) as u32;

            if x1 > x0 && y1 > y0 {
                raw_boxes.push((x0, y0, x1 - x0, y1 - y0, mean_score));
            }
        }
    }

    // Sort top-to-bottom (row-grouped within ~10px), then left-to-right.
    raw_boxes.sort_by(|a, b| {
        let row_a = a.1 / 12;
        let row_b = b.1 / 12;
        row_a.cmp(&row_b).then_with(|| a.0.cmp(&b.0))
    });
    raw_boxes
}

fn run_cls(cls: &mut Session, crop: &DynamicImage) -> Result<bool> {
    let resized = crop.resize_exact(192, 48, FilterType::Triangle).to_rgb8();
    let mut tensor = Array4::<f32>::zeros((1, 3, 48, 192));
    for y in 0..48 {
        for x in 0..192 {
            let p = resized.get_pixel(x as u32, y as u32);
            for c in 0..3 {
                tensor[[0, c, y, x]] = (p[c] as f32 / 255.0 - 0.5) / 0.5;
            }
        }
    }
    let outputs = cls
        .run(ort::inputs![TensorRef::from_array_view(&tensor)
            .map_err(|e| UiError::Platform(e.to_string()))?])
        .map_err(|e| UiError::Platform(e.to_string()))?;
    let (_, data) = outputs[0]
        .try_extract_tensor::<f32>()
        .map_err(|e| UiError::Platform(e.to_string()))?;
    Ok(data.len() >= 2 && data[1] > 0.9 && data[1] > data[0])
}

fn run_rec(rec: &mut Session, keys: &[String], crop: &DynamicImage) -> Result<(String, f32)> {
    let img_h = 48usize;
    let cw = crop.width().max(1) as f32;
    let ch = crop.height().max(1) as f32;
    let target_w = ((cw / ch) * img_h as f32).ceil().clamp(16.0, 960.0) as usize;
    let padded_w = target_w.div_ceil(8) * 8;

    let resized = crop
        .resize_exact(target_w as u32, img_h as u32, FilterType::Triangle)
        .to_rgb8();
    let mut tensor = Array4::<f32>::zeros((1, 3, img_h, padded_w));
    for y in 0..img_h {
        for x in 0..target_w {
            let p = resized.get_pixel(x as u32, y as u32);
            for c in 0..3 {
                tensor[[0, c, y, x]] = (p[c] as f32 / 255.0 - 0.5) / 0.5;
            }
        }
    }

    let outputs = rec
        .run(ort::inputs![TensorRef::from_array_view(&tensor)
            .map_err(|e| UiError::Platform(e.to_string()))?])
        .map_err(|e| UiError::Platform(format!("rec inference failed: {e}")))?;
    let (shape, data) = outputs[0]
        .try_extract_tensor::<f32>()
        .map_err(|e| UiError::Platform(format!("rec extract failed: {e}")))?;

    let steps = shape.get(1).copied().unwrap_or(0) as usize;
    let classes = shape.get(2).copied().unwrap_or(0) as usize;
    if steps == 0 || classes == 0 {
        return Ok((String::new(), 0.0));
    }

    let mut out = String::new();
    let mut prev_idx: Option<usize> = None;
    let mut conf_sum = 0.0f32;
    let mut conf_count = 0u32;

    for t in 0..steps {
        let row = &data[t * classes..(t + 1) * classes];
        let mut best_idx = 0usize;
        let mut best_val = f32::NEG_INFINITY;
        for (c, &val) in row.iter().enumerate() {
            if val > best_val {
                best_val = val;
                best_idx = c;
            }
        }

        if best_idx > 0 && Some(best_idx) != prev_idx {
            if let Some(ch_str) = keys.get(best_idx) {
                out.push_str(ch_str);
                conf_sum += best_val.clamp(0.0, 1.0);
                conf_count += 1;
            }
        }
        prev_idx = Some(best_idx);
    }

    let mean_conf = if conf_count > 0 {
        conf_sum / conf_count as f32
    } else {
        0.0
    };
    Ok((out, mean_conf))
}
