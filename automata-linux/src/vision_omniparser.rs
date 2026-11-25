//! Optional HTTP sidecar adapter for Microsoft OmniParser (`/parse/`),
//! converting normalized `[x1, y1, x2, y2]` icon & text detections into
//! pixel-space [`crate::vision::OcrBlock`] items without bundling Python.

use std::io::Cursor;
use std::time::Duration;

use base64::Engine as _;
use image::DynamicImage;
use serde::Deserialize;

use crate::vision::OcrBlock;
use crate::{Result, UiError};

const DEFAULT_OMNIPARSER_URL: &str = "http://127.0.0.1:8000";
const OMNIPARSER_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Deserialize)]
struct OmniParseResponse {
    #[serde(default)]
    parsed_content_list: Vec<OmniParsedItem>,
}

#[derive(Debug, Deserialize)]
struct OmniParsedItem {
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    bbox: [f32; 4],
    #[serde(default)]
    interactivity: bool,
    #[serde(default)]
    content: Option<String>,
}

/// Returns the configured OmniParser sidecar base URL when set in
/// `AUTOMATA_OMNIPARSER_URL` or defaults to `http://127.0.0.1:8000` if explicitly requested.
pub fn resolve_omniparser_url(allow_default: bool) -> Option<String> {
    if let Ok(raw) = std::env::var("AUTOMATA_OMNIPARSER_URL") {
        let trimmed = raw.trim().trim_end_matches('/');
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    allow_default.then(|| DEFAULT_OMNIPARSER_URL.to_string())
}

/// Sends an in-memory PNG screenshot to OmniParser `POST /parse/` and returns
/// pixel-coordinate [`OcrBlock`] entries for all parsed icons & text elements.
pub fn recognize_with_omniparser(img: &DynamicImage) -> Result<Vec<OcrBlock>> {
    let base_url = resolve_omniparser_url(true).ok_or_else(|| {
        UiError::Platform("AUTOMATA_OMNIPARSER_URL is empty".into())
    })?;
    let endpoint = format!("{base_url}/parse/");

    let mut png_bytes: Vec<u8> = Vec::new();
    img.write_to(&mut Cursor::new(&mut png_bytes), image::ImageFormat::Png)
        .map_err(|e| UiError::Platform(format!("failed to encode PNG for OmniParser: {e}")))?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(png_bytes);
    let img_w = img.width();
    let img_h = img.height();

    let body = crate::compositor::block_on_compositor(async move {
        let client = reqwest::Client::builder()
            .timeout(OMNIPARSER_TIMEOUT)
            .build()
            .map_err(|e| UiError::Platform(format!("build reqwest client for OmniParser: {e}")))?;
        let resp = client
            .post(&endpoint)
            .json(&serde_json::json!({ "base64_image": b64 }))
            .send()
            .await
            .map_err(|e| UiError::Platform(format!("OmniParser request to {endpoint} failed: {e}")))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| UiError::Platform(format!("read OmniParser response body: {e}")))?;
        if !status.is_success() {
            return Err(UiError::Platform(format!(
                "OmniParser {endpoint} returned HTTP {status}: {text}"
            ))
            .into());
        }
        Ok(text)
    })?;

    parse_omniparser_response(&body, img_w, img_h)
}

/// Converts OmniParser JSON (`parsed_content_list` with normalized `0..1` `[x1, y1, x2, y2]`
/// boxes) into pixel-space [`OcrBlock`] entries.
pub fn parse_omniparser_response(
    json_str: &str,
    img_width: u32,
    img_height: u32,
) -> Result<Vec<OcrBlock>> {
    let parsed: OmniParseResponse = serde_json::from_str(json_str)
        .map_err(|e| UiError::Platform(format!("invalid OmniParser JSON response: {e}")))?;
    let wf = f32::from(img_width.min(u16::MAX as u32) as u16);
    let hf = f32::from(img_height.min(u16::MAX as u32) as u16);

    let mut out = Vec::with_capacity(parsed.parsed_content_list.len());
    for item in parsed.parsed_content_list {
        let [x1, y1, x2, y2] = item.bbox;
        let px1 = (x1.clamp(0.0, 1.0) * wf).round() as i32;
        let py1 = (y1.clamp(0.0, 1.0) * hf).round() as i32;
        let px2 = (x2.clamp(0.0, 1.0) * wf).round() as i32;
        let py2 = (y2.clamp(0.0, 1.0) * hf).round() as i32;
        let w = (px2 - px1).max(1);
        let h = (py2 - py1).max(1);

        let label = item
            .content
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| {
                if item.kind.is_empty() {
                    "element".to_string()
                } else {
                    format!("[{}]", item.kind)
                }
            });

        out.push(OcrBlock {
            text: label,
            confidence: if item.interactivity { 0.95 } else { 0.85 },
            x: px1,
            y: py1,
            width: w,
            height: h,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_omniparser_normalized_bboxes_to_pixels() {
        let json = r#"{
            "som_image_base64": "AAAA",
            "latency": 0.42,
            "parsed_content_list": [
                {"type":"icon","bbox":[0.1,0.2,0.3,0.4],"interactivity":true,"content":"Close"},
                {"type":"text","bbox":[0.5,0.5,0.75,0.6],"interactivity":false,"content":"Submit"}
            ]
        }"#;
        let blocks = parse_omniparser_response(json, 1000, 500).expect("parse omniparser");
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].text, "Close");
        assert_eq!(
            (blocks[0].x, blocks[0].y, blocks[0].width, blocks[0].height),
            (100, 100, 200, 100)
        );
        assert_eq!(blocks[1].text, "Submit");
        assert_eq!(
            (blocks[1].x, blocks[1].y, blocks[1].width, blocks[1].height),
            (500, 250, 250, 50)
        );
    }
}
