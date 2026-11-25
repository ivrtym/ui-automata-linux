//! Strongly-typed desktop coordinate spaces, IEEE-754 safe window handles,
//! and unified screen-coordinate resolution across X11, XWayland, and Wayland.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use x11rb::connection::Connection;

use crate::UiError;
use crate::a11y::{MAX_SAFE_JS_HWND, synthetic_hwnd};
use crate::backend::{DisplayBackend, has_x11_display};
use crate::capture::{intersect_rect, monitors_have_gap};

/// Strongly-typed synthetic desktop window handle guaranteed to fit within
/// IEEE-754 double-precision exact integers (`1 ..= 2^53 - 1`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WindowId(u64);

impl WindowId {
    /// Validates and wraps a raw numeric window handle.
    pub fn new(raw: u64) -> Result<Self, UiError> {
        if raw == 0 || raw > MAX_SAFE_JS_HWND {
            return Err(UiError::Internal(format!(
                "window id {raw} is out of safe 53-bit range (1..={MAX_SAFE_JS_HWND})"
            )));
        }
        Ok(Self(raw))
    }

    /// Deterministically derives a safe `WindowId` from AT-SPI `(bus_name, path)`.
    pub fn from_synthetic(bus_name: &str, path: &str) -> Self {
        Self(synthetic_hwnd(bus_name, path).max(1).min(MAX_SAFE_JS_HWND))
    }

    /// Parses either `"0x..."` hex or decimal `u64` strings safely.
    pub fn parse_str(raw: &str) -> Result<Self, UiError> {
        let s = raw.trim();
        let parsed = if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
            u64::from_str_radix(hex, 16).map_err(|e| {
                UiError::Internal(format!("invalid hex window id {raw:?}: {e}"))
            })?
        } else {
            s.parse::<u64>().map_err(|e| {
                UiError::Internal(format!("invalid numeric window id {raw:?}: {e}"))
            })?
        };
        Self::new(parsed)
    }

    /// Parses a JSON value supplied as `"0x..."`, decimal string, or safe integer.
    pub fn parse_json_value(v: &serde_json::Value) -> Result<Self, UiError> {
        if let Some(n) = v.as_u64() {
            return Self::new(n);
        }
        if let Some(s) = v.as_str() {
            return Self::parse_str(s);
        }
        Err(UiError::Internal(format!(
            "expected window id as 53-bit integer or '0x...' string, got {v}"
        )))
    }

    pub fn as_u64(self) -> u64 {
        self.0
    }

    pub fn to_hex(self) -> String {
        format!("{:#x}", self.0)
    }
}

impl fmt::Display for WindowId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:#x}", self.0)
    }
}

impl Serialize for WindowId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for WindowId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let v = serde_json::Value::deserialize(deserializer)?;
        Self::parse_json_value(&v).map_err(serde::de::Error::custom)
    }
}

/// Zero-based logical monitor index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MonitorId(pub u32);

/// Coordinate reference frame for points and bounding boxes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(tag = "space", rename_all = "snake_case")]
pub enum CoordSpace {
    #[default]
    Screen,
    Window {
        window_id: WindowId,
    },
    Monitor {
        monitor_id: MonitorId,
    },
}

/// Point tagged with its explicit coordinate space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpacePoint {
    pub x: i32,
    pub y: i32,
    #[serde(flatten)]
    pub space: CoordSpace,
}

impl SpacePoint {
    pub fn screen(x: i32, y: i32) -> Self {
        Self {
            x,
            y,
            space: CoordSpace::Screen,
        }
    }

    pub fn in_window(window_id: WindowId, x: i32, y: i32) -> Self {
        Self {
            x,
            y,
            space: CoordSpace::Window { window_id },
        }
    }

    pub fn in_monitor(monitor_id: MonitorId, x: i32, y: i32) -> Self {
        Self {
            x,
            y,
            space: CoordSpace::Monitor { monitor_id },
        }
    }
}

/// Rectangle tagged with its explicit coordinate space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    #[serde(flatten)]
    pub space: CoordSpace,
}

impl SpaceRect {
    pub fn screen(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self {
            x,
            y,
            width,
            height,
            space: CoordSpace::Screen,
        }
    }
}

/// Physical/logical global desktop point after resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenPoint {
    pub x: i32,
    pub y: i32,
}

/// Physical/logical global desktop rectangle after resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// Descriptor for an active monitor / desktop output geometry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitorGeometry {
    pub id: MonitorId,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// Centralized coordinate transformation & validation helpers.
pub struct CoordResolver;

impl CoordResolver {
    /// Returns screen geometries for available logical outputs (best-effort X11 root fallback).
    pub fn monitors() -> Vec<MonitorGeometry> {
        if has_x11_display() {
            if let Ok((conn, screen_num)) = x11rb::connect(None) {
                let s = &conn.setup().roots[screen_num];
                return vec![MonitorGeometry {
                    id: MonitorId(0),
                    x: 0,
                    y: 0,
                    width: i32::from(s.width_in_pixels),
                    height: i32::from(s.height_in_pixels),
                }];
            }
        }
        Vec::new()
    }

    /// Resolves any [`SpacePoint`] into global [`ScreenPoint`] coordinates.
    pub fn to_screen_point<F>(point: SpacePoint, window_bounds: F) -> Result<ScreenPoint, UiError>
    where
        F: FnOnce(WindowId) -> Result<(i32, i32, i32, i32), UiError>,
    {
        match point.space {
            CoordSpace::Screen => Ok(ScreenPoint {
                x: point.x,
                y: point.y,
            }),
            CoordSpace::Window { window_id } => {
                let (wx, wy, _, _) = window_bounds(window_id)?;
                Ok(ScreenPoint {
                    x: wx.saturating_add(point.x),
                    y: wy.saturating_add(point.y),
                })
            }
            CoordSpace::Monitor { monitor_id } => {
                let monitors = Self::monitors();
                let mon = monitors
                    .into_iter()
                    .find(|m| m.id == monitor_id)
                    .ok_or_else(|| {
                        UiError::Internal(format!("monitor {} not found", monitor_id.0))
                    })?;
                Ok(ScreenPoint {
                    x: mon.x.saturating_add(point.x),
                    y: mon.y.saturating_add(point.y),
                })
            }
        }
    }

    /// Resolves any [`SpaceRect`] into global [`ScreenRect`] coordinates,
    /// intersecting against the desktop/monitor rectangle when gaps exist.
    pub fn to_screen_rect<F>(rect: SpaceRect, window_bounds: F) -> Result<ScreenRect, UiError>
    where
        F: FnOnce(WindowId) -> Result<(i32, i32, i32, i32), UiError>,
    {
        let (gx, gy) = match rect.space {
            CoordSpace::Screen => (rect.x, rect.y),
            CoordSpace::Window { window_id } => {
                let (wx, wy, _, _) = window_bounds(window_id)?;
                (wx.saturating_add(rect.x), wy.saturating_add(rect.y))
            }
            CoordSpace::Monitor { monitor_id } => {
                let monitors = Self::monitors();
                let mon = monitors
                    .iter()
                    .find(|m| m.id == monitor_id)
                    .ok_or_else(|| {
                        UiError::Internal(format!("monitor {} not found", monitor_id.0))
                    })?;
                let target = (
                    mon.x.saturating_add(rect.x),
                    mon.y.saturating_add(rect.y),
                    rect.width,
                    rect.height,
                );
                let mon_box = (mon.x, mon.y, mon.width, mon.height);
                if let Some((cx, cy, cw, ch)) = intersect_rect(target, mon_box) {
                    return Ok(ScreenRect {
                        x: cx,
                        y: cy,
                        width: cw,
                        height: ch,
                    });
                }
                return Err(UiError::Internal(
                    "rectangle lies outside monitor bounds".into(),
                ));
            }
        };

        let monitors = Self::monitors();
        if monitors.len() > 1 {
            let rects: Vec<(i32, i32, i32, i32)> =
                monitors.iter().map(|m| (m.x, m.y, m.width, m.height)).collect();
            if monitors_have_gap(&rects) {
                for m_rect in rects {
                    if let Some((cx, cy, cw, ch)) =
                        intersect_rect((gx, gy, rect.width, rect.height), m_rect)
                    {
                        return Ok(ScreenRect {
                            x: cx,
                            y: cy,
                            width: cw,
                            height: ch,
                        });
                    }
                }
            }
        }

        Ok(ScreenRect {
            x: gx,
            y: gy,
            width: rect.width,
            height: rect.height,
        })
    }

    /// Computes effective `ydotool --absolute` coordinate multiplier cleanly in one place.
    pub fn ydotool_abs_scale(
        display: Option<DisplayBackend>,
        xdg_current_desktop: Option<&str>,
        env_override: Option<&str>,
    ) -> f64 {
        if let Some(raw) = env_override {
            if let Ok(v) = raw.trim().parse::<f64>() {
                if v > 0.0 && v.is_finite() {
                    return v;
                }
            }
        }
        let is_gnome_wayland = display == Some(DisplayBackend::Wayland)
            && xdg_current_desktop
                .map(|v| v.to_ascii_lowercase().contains("gnome"))
                .unwrap_or(false);
        if is_gnome_wayland { 0.5 } else { 1.0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_id_enforces_53_bit_js_safety() {
        assert!(WindowId::new(0).is_err());
        assert!(WindowId::new(MAX_SAFE_JS_HWND + 1).is_err());
        let id = WindowId::new(MAX_SAFE_JS_HWND).unwrap();
        assert_eq!(WindowId::parse_str(&id.to_hex()).unwrap(), id);
        assert_eq!(WindowId::parse_str(&id.as_u64().to_string()).unwrap(), id);
        assert!(
            WindowId::parse_json_value(&serde_json::json!(MAX_SAFE_JS_HWND + 1)).is_err()
        );
    }

    #[test]
    fn resolves_window_and_screen_coordinates() {
        let wid = WindowId::new(0x1234).unwrap();
        let pt = SpacePoint::in_window(wid, 25, 40);
        let screen_pt =
            CoordResolver::to_screen_point(pt, |_| Ok((100, 200, 800, 600))).unwrap();
        assert_eq!(screen_pt, ScreenPoint { x: 125, y: 240 });

        let rect = SpaceRect {
            x: 10,
            y: 20,
            width: 300,
            height: 200,
            space: CoordSpace::Window { window_id: wid },
        };
        let screen_rect =
            CoordResolver::to_screen_rect(rect, |_| Ok((100, 200, 800, 600))).unwrap();
        assert_eq!(
            screen_rect,
            ScreenRect {
                x: 110,
                y: 220,
                width: 300,
                height: 200
            }
        );
    }

    #[test]
    fn computes_ydotool_abs_scale_for_gnome_wayland() {
        assert_eq!(
            CoordResolver::ydotool_abs_scale(Some(DisplayBackend::Wayland), Some("ubuntu:GNOME"), None),
            0.5
        );
        assert_eq!(
            CoordResolver::ydotool_abs_scale(Some(DisplayBackend::X11), Some("ubuntu:GNOME"), None),
            1.0
        );
        assert_eq!(
            CoordResolver::ydotool_abs_scale(
                Some(DisplayBackend::Wayland),
                Some("ubuntu:GNOME"),
                Some("1.25")
            ),
            1.25
        );
    }
}
