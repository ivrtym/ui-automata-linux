use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

use atspi::proxy::accessible::{AccessibleProxy, ObjectRefExt};
use atspi::proxy::bus::BusProxy;
use atspi::zbus;
use atspi::zbus::names::BusName;
use atspi::zbus::zvariant::{ObjectPath, OwnedObjectPath};
use atspi::{CoordType, Interface, ObjectRefOwned, Role, State};
use futures_util::stream::{self, StreamExt};

use crate::{Result, UiError};

/// Bounded D-Bus child fan-out concurrency used during AT-SPI tree snapshots.
pub const CHILD_READ_CONCURRENCY: usize = 16;

/// Shared synchronous bridge around `tokio` and the AT-SPI2 D-Bus connection.
pub struct A11yClient {
    rt: tokio::runtime::Runtime,
    dbus: zbus::Connection,
    pid_cache: Mutex<HashMap<String, u32>>,
    logged_denied_peers: Mutex<HashSet<String>>,
}

static GLOBAL_CLIENT: Mutex<Option<Arc<A11yClient>>> = Mutex::new(None);

impl A11yClient {
    /// Get or initialize the process-wide shared AT-SPI2 client.
    ///
    /// Unlike `OnceLock`, failed connections are not cached permanently so
    /// transient bus startup races can recover on retry.
    pub fn shared() -> Result<Arc<Self>> {
        if let Some(existing) = GLOBAL_CLIENT
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .cloned()
        {
            return Ok(existing);
        }

        let created = Arc::new(Self::connect()?);
        let mut guard = GLOBAL_CLIENT.lock().unwrap_or_else(|e| e.into_inner());
        let client = guard.get_or_insert_with(|| Arc::clone(&created));
        Ok(Arc::clone(client))
    }

    /// Drop the cached shared AT-SPI2 client so the next `shared()` call reconnects.
    pub fn reset_shared() {
        *GLOBAL_CLIENT.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// Run `op` using the shared AT-SPI2 client, automatically resetting and reconnecting once
    /// if D-Bus / `at-spi-registryd` reports a dropped connection.
    pub fn with_shared_retry<T, F>(mut op: F) -> Result<T>
    where
        F: FnMut(&Arc<Self>) -> Result<T>,
    {
        let client = Self::shared()?;
        match op(&client) {
            Ok(v) => Ok(v),
            Err(err) if is_connection_lost_error(&err) => {
                log::warn!("AT-SPI2 connection lost ({err}); reconnecting...");
                Self::reset_shared();
                let fresh = Self::shared()?;
                op(&fresh)
            }
            Err(err) => Err(err),
        }
    }

    fn connect() -> Result<Self> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .map_err(|e| UiError::Platform(format!("failed to create Tokio runtime: {e}")))?;

        // Connect directly to the AT-SPI2 bus rather than `atspi::AccessibilityConnection::new()`,
        // whose default `p2p` initialization calls `GetInterfaces` on every registered peer and
        // aborts the entire connection if any Snap app denies D-Bus access via AppArmor.
        let dbus = rt
            .block_on(async {
                let addr_str = resolve_atspi_bus_address().await?;
                let addr: zbus::Address = addr_str.as_str().try_into().map_err(|e| {
                    UiError::Platform(format!("invalid AT-SPI2 bus address '{addr_str}': {e}"))
                })?;
                zbus::connection::Builder::address(addr)
                    .map_err(|e| UiError::Platform(e.to_string()))?
                    .build()
                    .await
                    .map_err(|e| UiError::Platform(e.to_string()))
            })
            .map_err(|e| {
                UiError::Platform(format!(
                    "failed to connect to AT-SPI2 bus (is accessibility enabled?): {e}"
                ))
            })?;

        Ok(Self {
            rt,
            dbus,
            pid_cache: Mutex::new(HashMap::new()),
            logged_denied_peers: Mutex::new(HashSet::new()),
        })
    }

    pub fn block_on<F: std::future::Future>(&self, fut: F) -> F::Output {
        self.rt.block_on(fut)
    }

    pub fn dbus(&self) -> &zbus::Connection {
        &self.dbus
    }

    /// Build an `AccessibleProxy` for `(bus_name, path)` without property caching.
    pub async fn accessible_proxy<'a>(
        &'a self,
        bus_name: &'a str,
        path: &'a str,
    ) -> std::result::Result<AccessibleProxy<'a>, UiError> {
        let dest: BusName<'_> = bus_name
            .try_into()
            .map_err(|e| UiError::Platform(format!("invalid D-Bus name '{bus_name}': {e}")))?;
        let obj_path: ObjectPath<'_> = path
            .try_into()
            .map_err(|e| UiError::Platform(format!("invalid object path '{path}': {e}")))?;

        AccessibleProxy::builder(self.dbus())
            .destination(dest)
            .map_err(|e| UiError::Platform(e.to_string()))?
            .path(obj_path)
            .map_err(|e| UiError::Platform(e.to_string()))?
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
            .await
            .map_err(|e| UiError::Platform(e.to_string()))
    }

    /// Return all top-level accessible application proxies from the AT-SPI registry.
    pub fn root_applications(&self) -> Result<Vec<(String, String)>> {
        self.block_on(async {
            let root = self
                .accessible_proxy("org.a11y.atspi.Registry", "/org/a11y/atspi/accessible/root")
                .await?;
            let children = root
                .get_children()
                .await
                .map_err(|e| UiError::Platform(e.to_string()))?;
            let mut out = Vec::new();
            for obj in children {
                if let Some((bus, path)) = extract_ref(&obj) {
                    out.push((bus, path));
                }
            }
            Ok(out)
        })
    }

    /// Log an AppArmor/AccessDenied skip once per `(bus_name)` so window enumeration stays
    /// non-fatal without spamming logs.
    pub fn warn_skipped_peer(&self, bus_name: &str, err: &anyhow::Error) {
        let msg = err.to_string();
        if !is_access_denied_msg(&msg) {
            return;
        }
        let Ok(mut seen) = self.logged_denied_peers.lock() else {
            return;
        };
        if seen.insert(bus_name.to_string()) {
            let label = current_apparmor_label().unwrap_or_else(|| "unknown".into());
            log::warn!(
                "Skipping AT-SPI2 peer '{bus_name}' denied by AppArmor/D-Bus policy \
                 (caller label='{label}'; wrap launch with `aa-exec -p unconfined` to inspect Snap peers): {msg}"
            );
        }
    }

    /// Resolve the Unix PID owning `bus_name` via `org.freedesktop.DBus.GetConnectionUnixProcessID`.
    pub fn pid_for_bus(&self, bus_name: &str) -> Result<u32> {
        if let Some(pid) = self.pid_cache.lock().unwrap().get(bus_name).copied() {
            return Ok(pid);
        }

        let pid = self.block_on(async {
            let proxy = zbus::fdo::DBusProxy::new(self.dbus())
                .await
                .map_err(|e| UiError::Platform(e.to_string()))?;
            let name: BusName<'_> = bus_name
                .try_into()
                .map_err(|e| UiError::Platform(format!("invalid D-Bus name '{bus_name}': {e}")))?;
            proxy
                .get_connection_unix_process_id(name)
                .await
                .map_err(|e| UiError::Platform(format!("GetConnectionUnixProcessID({bus_name}): {e}")))
        })?;

        self.pid_cache
            .lock()
            .unwrap()
            .insert(bus_name.to_string(), pid);
        Ok(pid)
    }

    /// Probe whether `(bus_name, path)` has `State::Active`.
    pub fn is_active_window(&self, bus_name: &str, path: &str) -> bool {
        self.block_on(async {
            let Ok(proxy) = self.accessible_proxy(bus_name, path).await else {
                return false;
            };
            let Ok(states) = proxy.get_state().await else {
                return false;
            };
            states.contains(State::Active)
        })
    }

    /// Best-effort fetch of child `(bus_name, path)` pairs.
    pub fn children_refs(&self, bus_name: &str, path: &str) -> Result<Vec<(String, String)>> {
        self.block_on(async {
            let (children, _) = self.children_refs_bounded_async(bus_name, path, usize::MAX).await?;
            Ok(children)
        })
    }

    /// Bounded concurrent fetch of up to `limit` children for `(bus_name, path)`.
    ///
    /// Uses `ChildCount` + `GetChildAtIndex` fanned out with concurrency
    /// [`CHILD_READ_CONCURRENCY`] so oversized containers (e.g. 10,000+ DOM nodes)
    /// stop at `limit` without serializing one massive D-Bus `GetChildren` reply,
    /// while resolving WebKit well-known bus names via `GetNameOwner`.
    pub async fn children_refs_bounded_async(
        &self,
        bus_name: &str,
        path: &str,
        limit: usize,
    ) -> Result<(Vec<(String, String)>, bool)> {
        if limit == 0 {
            return Ok((Vec::new(), false));
        }
        let proxy = self.accessible_proxy(bus_name, path).await?;
        let reported = match proxy.child_count().await {
            Ok(c) if c > 0 => c as usize,
            Ok(_) => return Ok((Vec::new(), false)),
            Err(_) => {
                // Some toolkits don't expose ChildCount; fall back to GetChildren.
                let children = proxy
                    .get_children()
                    .await
                    .map_err(|e| UiError::Platform(e.to_string()))?;
                let total = children.len();
                let out: Vec<(String, String)> = children
                    .into_iter()
                    .filter_map(|obj| extract_ref(&obj))
                    .take(limit)
                    .collect();
                return Ok((out, total > limit));
            }
        };

        let attempt = reported.min(limit);
        let truncated = attempt < reported;

        // Fetch indexed children concurrently in bounded pipeline.
        let mut results: Vec<std::result::Result<Option<(String, String)>, ()>> =
            stream::iter(0..(attempt.min(i32::MAX as usize) as i32))
                .map(|idx| {
                    let proxy = &proxy;
                    let conn = self.dbus();
                    async move { fetch_child_at_index(proxy, conn, idx).await.map_err(|_| ()) }
                })
                .buffered(CHILD_READ_CONCURRENCY)
                .collect()
                .await;

        let all_failed = !results.is_empty() && results.iter().all(|r| r.is_err());
        if all_failed {
            // Fallback to GetChildren when GetChildAtIndex is unimplemented by a peer.
            let children = proxy
                .get_children()
                .await
                .map_err(|e| UiError::Platform(e.to_string()))?;
            let total = children.len();
            let out: Vec<(String, String)> = children
                .into_iter()
                .filter_map(|obj| extract_ref(&obj))
                .take(limit)
                .collect();
            return Ok((out, truncated || total > limit));
        }

        let mut out = Vec::with_capacity(attempt);
        for res in results.drain(..) {
            if let Ok(Some(pair)) = res {
                out.push(pair);
            }
        }
        Ok((out, truncated))
    }

    /// Return all top-level AT-SPI application `(bus_name, path)` pairs owned by `pid`
    /// (supports multi-root Chromium / Electron / Flatpak processes).
    pub fn app_roots_for_pid(&self, pid: u32) -> Result<Vec<(String, String)>> {
        let apps = self.root_applications()?;
        let mut matched = Vec::new();
        for (bus, path) in apps {
            if self.pid_for_bus(&bus).ok() == Some(pid) {
                matched.push((bus, path));
            }
        }
        Ok(matched)
    }

    /// Inspect basic window metadata for a child of an application.
    pub fn is_top_level_window(&self, bus_name: &str, path: &str) -> bool {
        self.block_on(async {
            let Ok(proxy) = self.accessible_proxy(bus_name, path).await else {
                return false;
            };
            let Ok(role) = proxy.get_role().await else {
                return false;
            };
            matches!(
                role,
                Role::Frame | Role::Window | Role::Dialog | Role::Alert | Role::FileChooser
            )
        })
    }

    /// Return true when `(bus_name, path)` is an active, visible AT-SPI2 tooltip element.
    pub fn is_visible_tooltip(&self, bus_name: &str, path: &str) -> bool {
        self.block_on(async {
            let Ok(proxy) = self.accessible_proxy(bus_name, path).await else {
                return false;
            };
            if !matches!(proxy.get_role().await, Ok(Role::ToolTip)) {
                return false;
            }
            let Ok(states) = proxy.get_state().await else {
                return false;
            };
            !states.contains(State::Defunct)
                && states.contains(State::Visible)
                && states.contains(State::Showing)
        })
    }

    /// Read `(role, name, bounds)` for quick filtering.
    pub fn quick_summary(&self, bus_name: &str, path: &str) -> Option<(Role, String, (i32, i32, i32, i32))> {
        self.block_on(async {
            let proxy = self.accessible_proxy(bus_name, path).await.ok()?;
            let role = proxy.get_role().await.ok()?;
            let name = proxy.name().await.unwrap_or_default();
            let ifaces = proxy.get_interfaces().await.ok()?;
            let bounds = if ifaces.contains(Interface::Component) {
                let comp = atspi::proxy::component::ComponentProxy::builder(self.dbus())
                    .destination(bus_name)
                    .ok()?
                    .path(path)
                    .ok()?
                    .cache_properties(zbus::proxy::CacheProperties::No)
                    .build()
                    .await
                    .ok()?;
                comp.get_extents(CoordType::Screen).await.unwrap_or((0, 0, 0, 0))
            } else {
                (0, 0, 0, 0)
            };
            Some((role, name, bounds))
        })
    }

    pub async fn proxy_from_ref<'a>(
        &'a self,
        obj: &'a ObjectRefOwned,
    ) -> std::result::Result<AccessibleProxy<'a>, UiError> {
        obj.as_accessible_proxy(self.dbus())
            .await
            .map_err(|e| UiError::Platform(e.to_string()))
    }
}

async fn fetch_child_at_index(
    proxy: &AccessibleProxy<'_>,
    conn: &zbus::Connection,
    index: i32,
) -> std::result::Result<Option<(String, String)>, zbus::Error> {
    let (name, path): (String, OwnedObjectPath) =
        proxy.inner().call("GetChildAtIndex", &(index,)).await?;
    let path_str = path.as_str();
    if name.is_empty() || path_str == "/org/a11y/atspi/null" {
        return Ok(None);
    }
    let resolved_bus = match BusName::try_from(name.as_str()) {
        Ok(BusName::Unique(_)) => name,
        Ok(well_known) => {
            if let Ok(dbus) = zbus::fdo::DBusProxy::new(conn).await {
                if let Ok(owner) = dbus.get_name_owner(well_known).await {
                    owner.to_string()
                } else {
                    name
                }
            } else {
                name
            }
        }
        Err(_) => name,
    };
    Ok(Some((resolved_bus, path_str.to_string())))
}

pub fn extract_ref(obj: &ObjectRefOwned) -> Option<(String, String)> {
    if obj.is_null() {
        return None;
    }
    let bus = obj.name_as_str()?.to_string();
    let path = obj.path_as_str().to_string();
    if bus.is_empty() || path == "/org/a11y/atspi/null" {
        return None;
    }
    Some((bus, path))
}

/// Largest integer guaranteed to round-trip losslessly through JSON/JavaScript IEEE-754 doubles (`2^53 - 1`).
pub const MAX_SAFE_JS_HWND: u64 = (1u64 << 53) - 1;

/// Stable non-zero 53-bit-safe window handle derived from `(bus_name, object_path)`.
///
/// Masked to [`MAX_SAFE_JS_HWND`] so JSON/JavaScript MCP hosts parsing numbers as IEEE-754
/// doubles never round away low bits (computer-use-linux issue #341).
pub fn synthetic_hwnd(bus_name: &str, path: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bus_name.hash(&mut hasher);
    path.hash(&mut hasher);
    let h = hasher.finish() & MAX_SAFE_JS_HWND;
    if h == 0 { 1 } else { h }
}

async fn resolve_atspi_bus_address() -> std::result::Result<String, UiError> {
    let session_err = match zbus::Connection::session().await {
        Ok(session_bus) => match BusProxy::new(&session_bus).await {
            Ok(proxy) => match proxy.get_address().await {
                Ok(addr) if !addr.trim().is_empty() => return Ok(addr),
                Ok(_) => UiError::Platform("org.a11y.Bus returned empty address".into()),
                Err(e) => UiError::Platform(e.to_string()),
            },
            Err(e) => UiError::Platform(e.to_string()),
        },
        Err(e) => UiError::Platform(e.to_string()),
    };

    if let Ok(env_addr) = std::env::var("AT_SPI_BUS_ADDRESS") {
        let trimmed = env_addr.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }

    Err(session_err)
}

pub(crate) fn is_access_denied_msg(msg: &str) -> bool {
    let lower = msg.to_ascii_lowercase();
    lower.contains("org.freedesktop.dbus.error.accessdenied")
        || lower.contains("apparmor policy prevents")
}

/// Read the active Linux AppArmor label for the current process, if any.
pub fn current_apparmor_label() -> Option<String> {
    for path in ["/proc/self/attr/apparmor/current", "/proc/self/attr/current"] {
        if let Ok(raw) = std::fs::read_to_string(path) {
            let trimmed = raw.trim_matches(['\0', '\n', '\r', ' ', '\t']);
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

pub(crate) fn is_connection_lost_error(err: &anyhow::Error) -> bool {
    is_connection_lost_msg(&err.to_string())
}

pub(crate) fn is_connection_lost_msg(msg: &str) -> bool {
    let lower = msg.to_ascii_lowercase();
    [
        "disconnected",
        "connection closed",
        "broken pipe",
        "connection reset",
        "eof",
        "serviceunknown",
        "namehasnoowner",
        "org.a11y.atspi.registry",
        "input/output error",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// Normalise AT-SPI2 `Role` to selector-friendly role names aligned with `ui-automata`.
pub fn normalize_role(role: Role, editable: bool) -> String {
    match role {
        Role::Frame | Role::Window => "window".to_string(),
        Role::Dialog | Role::Alert | Role::FileChooser | Role::ColorChooser | Role::FontChooser => {
            "dialog".to_string()
        }
        Role::Button | Role::ToggleButton => "button".to_string(),
        Role::MenuBar => "menu bar".to_string(),
        Role::MenuItem | Role::CheckMenuItem | Role::RadioMenuItem | Role::TearoffMenuItem => {
            "menu item".to_string()
        }
        Role::ListItem => "list item".to_string(),
        Role::TreeItem => "tree item".to_string(),
        Role::CheckBox => "check box".to_string(),
        Role::RadioButton => "radio button".to_string(),
        Role::ComboBox => "combo box".to_string(),
        Role::PageTab => "tab item".to_string(),
        Role::PageTabList => "tab".to_string(),
        Role::Panel | Role::Filler | Role::RootPane | Role::LayeredPane | Role::OptionPane => {
            "pane".to_string()
        }
        Role::Entry | Role::PasswordText => "edit".to_string(),
        Role::Text => {
            if editable {
                "edit".to_string()
            } else {
                "text".to_string()
            }
        }
        Role::DocumentWeb | Role::DocumentFrame | Role::DocumentText => "document".to_string(),
        Role::PopupMenu => "menu".to_string(),
        Role::ToolTip => "tool tip".to_string(),
        other => other.name().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{normalize_role, synthetic_hwnd};
    use atspi::Role;

    #[test]
    fn normalizes_common_roles() {
        assert_eq!(normalize_role(Role::Frame, false), "window");
        assert_eq!(normalize_role(Role::Dialog, false), "dialog");
        assert_eq!(normalize_role(Role::Button, false), "button");
        assert_eq!(normalize_role(Role::Text, false), "text");
        assert_eq!(normalize_role(Role::Text, true), "edit");
        assert_eq!(normalize_role(Role::Entry, false), "edit");
        assert_eq!(normalize_role(Role::ToolTip, false), "tool tip");
    }

    #[test]
    fn synthetic_hwnd_is_deterministic_and_distinct() {
        let a1 = synthetic_hwnd(":1.10", "/org/a11y/atspi/accessible/1");
        let a2 = synthetic_hwnd(":1.10", "/org/a11y/atspi/accessible/1");
        let b = synthetic_hwnd(":1.10", "/org/a11y/atspi/accessible/2");
        assert_eq!(a1, a2);
        assert_ne!(a1, 0);
        assert_ne!(a1, b);
        assert!(a1 <= super::MAX_SAFE_JS_HWND);
        assert!(b <= super::MAX_SAFE_JS_HWND);
    }

    #[test]
    fn detects_dbus_connection_loss_errors() {
        assert!(super::is_connection_lost_msg(
            "org.freedesktop.DBus.Error.Disconnected: Connection closed"
        ));
        assert!(super::is_connection_lost_msg("Broken pipe (os error 32)"));
        assert!(super::is_connection_lost_msg(
            "org.freedesktop.DBus.Error.ServiceUnknown: org.a11y.atspi.Registry"
        ));
        assert!(!super::is_connection_lost_msg(
            "anchor 'win' not found in application windows"
        ));
    }

    #[test]
    fn detects_apparmor_access_denied_errors() {
        assert!(super::is_access_denied_msg(
            "ZBus Error: MethodError(OwnedErrorName(\"org.freedesktop.DBus.Error.AccessDenied\"), Some(\"An AppArmor policy prevents this sender from sending this message to this recipient\"))"
        ));
        assert!(!super::is_access_denied_msg(
            "org.freedesktop.DBus.Error.ServiceUnknown"
        ));
    }
}
