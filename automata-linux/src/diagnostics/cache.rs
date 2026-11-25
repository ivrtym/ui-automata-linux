//! Lightweight TTL probe cache for compositor & accessibility diagnostics plus
//! `at-spi2-core >= 2.62` `gsettings` schema compatibility helpers.

use std::collections::HashMap;
use std::hash::Hash;
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::runtime::command::run_bounded;

/// Default TTL used for repeated environment & compositor probes.
pub const DEFAULT_PROBE_TTL: Duration = Duration::from_secs(5);

struct CacheEntry<V> {
    inserted_at: Instant,
    value: V,
}

/// Thread-safe bounded-TTL cache avoiding redundant IPC / CLI probes during
/// hot paths such as `doctor` and window enumeration.
pub struct ProbeCache<K, V> {
    ttl: Duration,
    entries: Mutex<HashMap<K, CacheEntry<V>>>,
}

impl<K, V> ProbeCache<K, V>
where
    K: Eq + Hash,
    V: Clone,
{
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// Returns a cached value if present and fresher than `ttl`, otherwise runs
    /// `compute` and caches the result.
    pub fn get_or_insert_with(&self, key: K, compute: impl FnOnce() -> V) -> V {
        let now = Instant::now();
        if let Ok(guard) = self.entries.lock() {
            if let Some(entry) = guard.get(&key) {
                if now.duration_since(entry.inserted_at) < self.ttl {
                    return entry.value.clone();
                }
            }
        }
        let fresh = compute();
        if let Ok(mut guard) = self.entries.lock() {
            guard.insert(
                key,
                CacheEntry {
                    inserted_at: Instant::now(),
                    value: fresh.clone(),
                },
            );
        }
        fresh
    }

    pub fn invalidate(&self, key: &K) {
        if let Ok(mut guard) = self.entries.lock() {
            guard.remove(key);
        }
    }

    pub fn clear(&self) {
        if let Ok(mut guard) = self.entries.lock() {
            guard.clear();
        }
    }
}

/// Summary of AT-SPI2 toolkit accessibility enablement across legacy (`< 2.62`)
/// and modern (`>= 2.62`) GSettings schemas.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AtSpiToolkitProbe {
    pub schema_found: bool,
    pub enabled: bool,
    pub schema_used: &'static str,
    pub detail: String,
}

/// Probes GSettings for AT-SPI2 toolkit enablement, supporting both
/// `org.gnome.desktop.interface toolkit-accessibility` (traditional) and
/// `org.a11y.interface screen-reader-enabled` (`at-spi2-core >= 2.62` / GNOME 48+).
pub fn probe_atspi_toolkit_enabled() -> AtSpiToolkitProbe {
    let probes = [
        ("org.gnome.desktop.interface", "toolkit-accessibility"),
        ("org.a11y.interface", "screen-reader-enabled"),
    ];

    for (schema, key) in probes {
        let mut cmd = Command::new("gsettings");
        cmd.args(["get", schema, key]);
        if let Ok(out) = run_bounded(cmd, "gsettings accessibility probe", Duration::from_secs(2), 4096) {
            if out.status.success() {
                let raw = String::from_utf8_lossy(&out.stdout);
                if let Some(val) = parse_gsettings_bool(&raw) {
                    return AtSpiToolkitProbe {
                        schema_found: true,
                        enabled: val,
                        schema_used: schema,
                        detail: format!("{schema} {key} = {val}"),
                    };
                }
            }
        }
    }

    AtSpiToolkitProbe {
        schema_found: false,
        enabled: false,
        schema_used: "none",
        detail: "neither org.gnome.desktop.interface toolkit-accessibility nor org.a11y.interface screen-reader-enabled could be read via gsettings".into(),
    }
}

/// Parses `gsettings get` boolean stdout (`"true\n"`, `"false\n"`).
pub fn parse_gsettings_bool(stdout: &str) -> Option<bool> {
    match stdout.trim() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn probe_cache_reuses_fresh_entries_and_invalidates() {
        let cache = ProbeCache::new(Duration::from_secs(30));
        let calls = AtomicUsize::new(0);

        let a = cache.get_or_insert_with("compositor", || {
            calls.fetch_add(1, Ordering::Relaxed);
            "hyprland".to_string()
        });
        let b = cache.get_or_insert_with("compositor", || {
            calls.fetch_add(1, Ordering::Relaxed);
            "changed".to_string()
        });
        assert_eq!(a, "hyprland");
        assert_eq!(b, "hyprland");
        assert_eq!(calls.load(Ordering::Relaxed), 1);

        cache.invalidate(&"compositor");
        let c = cache.get_or_insert_with("compositor", || {
            calls.fetch_add(1, Ordering::Relaxed);
            "sway".to_string()
        });
        assert_eq!(c, "sway");
        assert_eq!(calls.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn parses_gsettings_boolean_stdout() {
        assert_eq!(parse_gsettings_bool("true\n"), Some(true));
        assert_eq!(parse_gsettings_bool(" false "), Some(false));
        assert_eq!(parse_gsettings_bool("'invalid'"), None);
    }
}
