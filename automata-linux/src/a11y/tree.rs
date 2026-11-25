//! Bounded, concurrent AT-SPI2 tree snapshotting, multi-root window aggregation,
//! selector query resolution, and automatic oversized-tree JSON spill.

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use atspi::proxy::proxy_ext::ProxyExt;
use atspi::{CoordType, Interface, Role, State};
use futures_util::stream::{self, StreamExt};

use crate::a11y::client::{A11yClient, CHILD_READ_CONCURRENCY, normalize_role, synthetic_hwnd};
use crate::a11y::element::LinuxElement;
use crate::a11y::encoding::{
    INLINE_TREE_RESULT_BYTES, clean_optional_text, spill_json_if_oversized, strip_placeholder_text,
};
use crate::{Desktop, Result, UiError};

/// Default depth ceiling for a single tree snapshot.
pub const DEFAULT_SNAPSHOT_MAX_DEPTH: usize = 32;
/// Hard depth ceiling regardless of caller request.
pub const HARD_SNAPSHOT_MAX_DEPTH: usize = 64;
/// Default node budget per snapshot.
pub const DEFAULT_SNAPSHOT_MAX_NODES: usize = 1000;
/// Node budget used when evaluating selectors via concurrent wave-BFS.
pub const FIND_SNAPSHOT_MAX_NODES: usize = 5000;
/// Hard node ceiling regardless of caller request.
pub const HARD_SNAPSHOT_MAX_NODES: usize = 10_000;
/// Soft deadline for full-tree traversal before returning partial results.
pub const DEFAULT_SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(10);
/// Extra grace allowing an in-flight batch wave to land cleanly after deadline.
pub const SNAPSHOT_HARD_GRACE: Duration = Duration::from_secs(2);

/// Bounds governing a concurrent AT-SPI2 snapshot traversal.
#[derive(Debug, Clone)]
pub struct SnapshotLimits {
    pub max_nodes: usize,
    pub max_depth: usize,
    pub timeout: Duration,
    pub grace: Duration,
}

impl Default for SnapshotLimits {
    fn default() -> Self {
        Self {
            max_nodes: DEFAULT_SNAPSHOT_MAX_NODES,
            max_depth: DEFAULT_SNAPSHOT_MAX_DEPTH,
            timeout: DEFAULT_SNAPSHOT_TIMEOUT,
            grace: SNAPSHOT_HARD_GRACE,
        }
    }
}

impl SnapshotLimits {
    /// Clamps `max_nodes` and `max_depth` against hard safety ceilings.
    pub fn clamped(max_nodes: usize, max_depth: usize, timeout: Duration) -> Self {
        Self {
            max_nodes: max_nodes.clamp(1, HARD_SNAPSHOT_MAX_NODES),
            max_depth: max_depth.clamp(1, HARD_SNAPSHOT_MAX_DEPTH),
            timeout,
            grace: SNAPSHOT_HARD_GRACE,
        }
    }
}

/// Output of a bounded snapshot traversal, preserving partial trees when
/// interrupted by depth, node, or timeout budgets.
#[derive(Debug, Clone)]
pub struct SnapshotTreeResult {
    pub root: ElementNode,
    pub node_count: usize,
    pub truncated: bool,
    pub truncation_reason: Option<String>,
}

/// A node in the AT-SPI2 element tree.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ElementNode {
    pub role: String,
    #[serde(default)]
    pub name: String,
    #[serde(rename = "id", default, skip_serializing_if = "Option::is_none")]
    pub automation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<ElementNode>,
}

type R<T> = std::result::Result<T, ui_automata::AutomataError>;

impl ui_automata::Element for ElementNode {
    fn name(&self) -> Option<String> {
        if self.name.is_empty() {
            None
        } else {
            Some(self.name.clone())
        }
    }
    fn role(&self) -> String {
        self.role.clone()
    }
    fn automation_id(&self) -> Option<String> {
        self.automation_id.clone()
    }
    fn bounds(&self) -> R<(i32, i32, i32, i32)> {
        Ok((self.x, self.y, self.width, self.height))
    }
    fn children(&self) -> R<Vec<Self>> {
        Ok(self.children.clone())
    }
    fn text(&self) -> R<String> {
        Ok(self.text.clone().unwrap_or_default())
    }

    fn inner_text(&self) -> R<String> {
        Ok(String::new())
    }
    fn is_enabled(&self) -> R<bool> {
        Ok(true)
    }
    fn is_visible(&self) -> R<bool> {
        Ok(true)
    }
    fn process_id(&self) -> R<u32> {
        Ok(0)
    }
    fn click(&self) -> R<()> {
        unimplemented!("snapshot")
    }
    fn double_click(&self) -> R<()> {
        unimplemented!("snapshot")
    }
    fn hover(&self) -> R<()> {
        unimplemented!("snapshot")
    }
    fn click_at(&self, _: f64, _: f64, _: ui_automata::ClickType) -> R<()> {
        unimplemented!("snapshot")
    }
    fn type_text(&self, _: &str) -> R<()> {
        unimplemented!("snapshot")
    }
    fn press_key(&self, _: &str) -> R<()> {
        unimplemented!("snapshot")
    }
    fn set_value(&self, _: &str) -> R<()> {
        unimplemented!("snapshot")
    }
    fn focus(&self) -> R<()> {
        unimplemented!("snapshot")
    }
    fn scroll_into_view(&self) -> R<()> {
        unimplemented!("snapshot")
    }
    fn activate_window(&self) -> R<()> {
        unimplemented!("snapshot")
    }
    fn minimize_window(&self) -> R<()> {
        unimplemented!("snapshot")
    }
    fn close(&self) -> R<()> {
        unimplemented!("snapshot")
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AncestorInfo {
    pub name: Option<String>,
    pub role: String,
    pub automation_id: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SiblingInfo {
    pub name: Option<String>,
    pub role: String,
    pub value: Option<String>,
    pub enabled: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ElementFindResult {
    pub name: Option<String>,
    pub role: String,
    pub value: Option<String>,
    pub enabled: bool,
    pub bounds: Option<(i32, i32, i32, i32)>,
    pub automation_id: Option<String>,
    pub pid: u32,
    pub ancestors: Vec<AncestorInfo>,
    pub siblings: Vec<SiblingInfo>,
}

fn matches_window_filters(
    win: &LinuxElement,
    pid: Option<u32>,
    title: Option<&str>,
    automation_id: Option<&str>,
    process: Option<&str>,
) -> bool {
    if let Some(p) = pid {
        if win.process_id().ok() != Some(p) {
            return false;
        }
    }
    if let Some(proc_filter) = process {
        let Some(pname) = win.process_name() else {
            return false;
        };
        if !ui_automata::process_names_match(&pname, proc_filter) {
            return false;
        }
    }
    if let Some(id_filter) = automation_id {
        if win.automation_id().as_deref() != Some(id_filter) {
            return false;
        }
    }
    if let Some(t) = title {
        let wname = win.name_or_empty();
        if wname != t && !wname.to_lowercase().contains(&t.to_lowercase()) {
            return false;
        }
    }
    true
}

pub fn find_matching_window(
    pid: Option<u32>,
    title: Option<&str>,
    automation_id: Option<&str>,
    process: Option<&str>,
    hwnd: Option<u64>,
) -> Result<LinuxElement> {
    let desktop = Desktop::new();
    find_matching_window_with(&desktop, pid, title, automation_id, process, hwnd)
}

pub fn find_matching_window_with(
    desktop: &Desktop,
    pid: Option<u32>,
    title: Option<&str>,
    automation_id: Option<&str>,
    process: Option<&str>,
    hwnd: Option<u64>,
) -> Result<LinuxElement> {
    let mut matches =
        find_matching_windows_with(desktop, pid, title, automation_id, process, hwnd)?;
    Ok(matches.remove(0))
}

/// Returns all windows matching the given filters (supporting multi-root /
/// multi-window apps sharing a PID or process name).
pub fn find_matching_windows_with(
    desktop: &Desktop,
    pid: Option<u32>,
    title: Option<&str>,
    automation_id: Option<&str>,
    process: Option<&str>,
    hwnd: Option<u64>,
) -> Result<Vec<LinuxElement>> {
    if pid.is_none()
        && title.is_none()
        && automation_id.is_none()
        && process.is_none()
        && hwnd.is_none()
    {
        return Err(
            UiError::Internal("supply pid, title, automation_id, process, or hwnd".into()).into(),
        );
    }

    let windows = desktop.application_windows()?;
    let matched: Vec<LinuxElement> = windows
        .into_iter()
        .filter(|win| {
            if let Some(h) = hwnd {
                return synthetic_hwnd(win.bus_name(), win.object_path()) == h;
            }
            matches_window_filters(win, pid, title, automation_id, process)
        })
        .collect();

    if matched.is_empty() {
        return Err(UiError::Internal("matching window not found".into()).into());
    }
    Ok(matched)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WaveReadMode {
    Full,
    SelectorOnly,
}

#[derive(Debug)]
struct FlatNode {
    parent_idx: Option<usize>,
    bus_name: String,
    path: String,
    role: String,
    name: String,
    automation_id: Option<String>,
    text: Option<String>,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    enabled: bool,
}

/// Indexed node stored inside [`SnapshotArena`] for O(1) parent/child navigation
/// and zero-RPC selector evaluation.
#[derive(Debug, Clone)]
struct ArenaNode {
    parent_idx: Option<usize>,
    children: Vec<usize>,
    bus_name: String,
    path: String,
    role: String,
    name: String,
    automation_id: Option<String>,
    text: Option<String>,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    enabled: bool,
}

/// Flat arena holding a concurrent AT-SPI2 snapshot or deserialized `ElementNode` tree.
#[derive(Debug, Clone)]
struct SnapshotArena {
    pid: u32,
    nodes: Vec<ArenaNode>,
}

impl SnapshotArena {
    fn from_element_node(root: &ElementNode) -> Self {
        let mut nodes = Vec::new();
        fn push_rec(node: &ElementNode, parent_idx: Option<usize>, out: &mut Vec<ArenaNode>) -> usize {
            let my_idx = out.len();
            out.push(ArenaNode {
                parent_idx,
                children: Vec::with_capacity(node.children.len()),
                bus_name: String::new(),
                path: String::new(),
                role: node.role.clone(),
                name: node.name.clone(),
                automation_id: node.automation_id.clone(),
                text: node.text.clone(),
                x: node.x,
                y: node.y,
                width: node.width,
                height: node.height,
                enabled: true,
            });
            let mut child_indices = Vec::with_capacity(node.children.len());
            for child in &node.children {
                child_indices.push(push_rec(child, Some(my_idx), out));
            }
            out[my_idx].children = child_indices;
            my_idx
        }
        push_rec(root, None, &mut nodes);
        Self { pid: 0, nodes }
    }
}

/// Lightweight handle over [`SnapshotArena`] implementing [`ui_automata::Element`]
/// with O(1) `parent()` and `children()` navigation.
#[derive(Clone)]
struct SnapshotNodeRef {
    arena: Arc<SnapshotArena>,
    idx: usize,
}

impl ui_automata::Element for SnapshotNodeRef {
    fn name(&self) -> Option<String> {
        let n = &self.arena.nodes[self.idx];
        if n.name.is_empty() {
            None
        } else {
            Some(n.name.clone())
        }
    }
    fn role(&self) -> String {
        self.arena.nodes[self.idx].role.clone()
    }
    fn automation_id(&self) -> Option<String> {
        self.arena.nodes[self.idx].automation_id.clone()
    }
    fn bounds(&self) -> R<(i32, i32, i32, i32)> {
        let n = &self.arena.nodes[self.idx];
        Ok((n.x, n.y, n.width, n.height))
    }
    fn children(&self) -> R<Vec<Self>> {
        Ok(self.arena.nodes[self.idx]
            .children
            .iter()
            .map(|&cidx| Self {
                arena: Arc::clone(&self.arena),
                idx: cidx,
            })
            .collect())
    }
    fn parent(&self) -> Option<Self> {
        self.arena.nodes[self.idx].parent_idx.map(|pidx| Self {
            arena: Arc::clone(&self.arena),
            idx: pidx,
        })
    }
    fn text(&self) -> R<String> {
        Ok(self.arena.nodes[self.idx].text.clone().unwrap_or_default())
    }
    fn inner_text(&self) -> R<String> {
        Ok(String::new())
    }
    fn is_enabled(&self) -> R<bool> {
        Ok(self.arena.nodes[self.idx].enabled)
    }
    fn is_visible(&self) -> R<bool> {
        Ok(true)
    }
    fn process_id(&self) -> R<u32> {
        Ok(self.arena.pid)
    }
    fn click(&self) -> R<()> {
        unimplemented!("snapshot")
    }
    fn double_click(&self) -> R<()> {
        unimplemented!("snapshot")
    }
    fn hover(&self) -> R<()> {
        unimplemented!("snapshot")
    }
    fn click_at(&self, _: f64, _: f64, _: ui_automata::ClickType) -> R<()> {
        unimplemented!("snapshot")
    }
    fn type_text(&self, _: &str) -> R<()> {
        unimplemented!("snapshot")
    }
    fn press_key(&self, _: &str) -> R<()> {
        unimplemented!("snapshot")
    }
    fn set_value(&self, _: &str) -> R<()> {
        unimplemented!("snapshot")
    }
    fn focus(&self) -> R<()> {
        unimplemented!("snapshot")
    }
    fn scroll_into_view(&self) -> R<()> {
        unimplemented!("snapshot")
    }
    fn activate_window(&self) -> R<()> {
        unimplemented!("snapshot")
    }
    fn minimize_window(&self) -> R<()> {
        unimplemented!("snapshot")
    }
    fn close(&self) -> R<()> {
        unimplemented!("snapshot")
    }
}

struct SnapshotArenaResult {
    arena: Arc<SnapshotArena>,
    truncated: bool,
    truncation_reason: Option<String>,
}

#[derive(Debug)]
struct QueuedRef {
    parent_idx: Option<usize>,
    depth: usize,
    bus_name: String,
    path: String,
}

#[derive(Debug)]
struct ReadWaveNode {
    parent_idx: Option<usize>,
    depth: usize,
    bus_name: String,
    path: String,
    role: String,
    name: String,
    automation_id: Option<String>,
    text: Option<String>,
    bounds: (i32, i32, i32, i32),
    enabled: bool,
    children: Vec<(String, String)>,
    children_truncated: bool,
}

/// Runs a concurrent wave-BFS traversal over `root` within `limits`,
/// gracefully returning partial results on node, depth, or deadline caps.
pub fn snapshot_tree_with_limits(
    root: &LinuxElement,
    limits: &SnapshotLimits,
) -> SnapshotTreeResult {
    let arena_res = snapshot_arena_with_limits(root, limits, WaveReadMode::Full);
    let node_count = arena_res.arena.nodes.len();
    let root_node = assemble_element_node(&arena_res.arena, 0);
    SnapshotTreeResult {
        root: root_node,
        node_count,
        truncated: arena_res.truncated,
        truncation_reason: arena_res.truncation_reason,
    }
}

fn snapshot_arena_with_limits(
    root: &LinuxElement,
    limits: &SnapshotLimits,
    mode: WaveReadMode,
) -> SnapshotArenaResult {
    let max_nodes = limits.max_nodes.clamp(1, HARD_SNAPSHOT_MAX_NODES);
    let max_depth = limits.max_depth.clamp(1, HARD_SNAPSHOT_MAX_DEPTH);
    let wayland_offset = if mode == WaveReadMode::Full {
        root.wayland_window_offset()
    } else {
        (0, 0)
    };
    let pid = root.process_id().unwrap_or(0);

    let mut arena: Vec<FlatNode> = Vec::new();
    let mut visited: HashSet<(String, String)> = HashSet::new();
    let mut queue: VecDeque<QueuedRef> = VecDeque::new();
    let mut truncated = false;
    let mut truncation_reason: Option<String> = None;

    let root_key = (root.bus_name().to_string(), root.object_path().to_string());
    visited.insert(root_key.clone());
    queue.push_back(QueuedRef {
        parent_idx: None,
        depth: 0,
        bus_name: root_key.0,
        path: root_key.1,
    });

    let soft_deadline = Instant::now() + limits.timeout;
    let hard_deadline = tokio::time::Instant::from_std(soft_deadline + limits.grace);

    root.client.block_on(async {
        while !queue.is_empty() && arena.len() < max_nodes {
            if Instant::now() >= soft_deadline {
                truncated = true;
                truncation_reason.get_or_insert_with(|| {
                    format!(
                        "snapshot timeout after {}s ({} nodes read)",
                        limits.timeout.as_secs(),
                        arena.len()
                    )
                });
                break;
            }

            let available_slots = max_nodes.saturating_sub(arena.len());
            let wave_size = CHILD_READ_CONCURRENCY.min(queue.len()).min(available_slots);
            let mut wave = Vec::with_capacity(wave_size);
            for _ in 0..wave_size {
                if let Some(item) = queue.pop_front() {
                    wave.push(item);
                }
            }

            let remaining_after_wave = max_nodes.saturating_sub(arena.len() + wave.len());
            let fetch_wave = stream::iter(wave.into_iter().map(|item| {
                let client = &root.client;
                let max_depth = max_depth;
                let per_parent_cap = remaining_after_wave.max(1);
                async move {
                    read_wave_node(client, item, max_depth, per_parent_cap, mode).await
                }
            }))
            .buffered(CHILD_READ_CONCURRENCY)
            .collect::<Vec<_>>();

            let Ok(wave_nodes) = tokio::time::timeout_at(hard_deadline, fetch_wave).await else {
                truncated = true;
                truncation_reason.get_or_insert_with(|| {
                    format!(
                        "snapshot wave timed out past grace window ({} nodes read)",
                        arena.len()
                    )
                });
                break;
            };

            for maybe_node in wave_nodes {
                let Some(node) = maybe_node else {
                    continue;
                };
                if arena.len() >= max_nodes {
                    truncated = true;
                    truncation_reason.get_or_insert_with(|| {
                        format!("node budget reached ({max_nodes} nodes)")
                    });
                    break;
                }
                if node.children_truncated {
                    truncated = true;
                    truncation_reason.get_or_insert_with(|| {
                        if node.depth >= max_depth {
                            format!("max depth reached ({max_depth})")
                        } else {
                            format!("child read truncated at node budget ({max_nodes})")
                        }
                    });
                }

                let (nx, ny) = if node.bounds.2 > 0 && node.bounds.3 > 0 {
                    (
                        node.bounds.0.saturating_add(wayland_offset.0),
                        node.bounds.1.saturating_add(wayland_offset.1),
                    )
                } else {
                    (node.bounds.0, node.bounds.1)
                };
                let my_idx = arena.len();
                arena.push(FlatNode {
                    parent_idx: node.parent_idx,
                    bus_name: node.bus_name,
                    path: node.path,
                    role: node.role,
                    name: node.name,
                    automation_id: node.automation_id,
                    text: node.text,
                    x: nx,
                    y: ny,
                    width: node.bounds.2,
                    height: node.bounds.3,
                    enabled: node.enabled,
                });

                for (cbus, cpath) in node.children {
                    if arena.len() + queue.len() >= max_nodes {
                        truncated = true;
                        truncation_reason.get_or_insert_with(|| {
                            format!("node budget reached ({max_nodes} nodes)")
                        });
                        break;
                    }
                    let key = (cbus, cpath);
                    if visited.insert(key.clone()) {
                        queue.push_back(QueuedRef {
                            parent_idx: Some(my_idx),
                            depth: node.depth + 1,
                            bus_name: key.0,
                            path: key.1,
                        });
                    }
                }
            }
        }

        if !queue.is_empty() && !truncated {
            truncated = true;
            truncation_reason.get_or_insert_with(|| format!("node budget reached ({max_nodes} nodes)"));
        }
    });

    if arena.is_empty() {
        arena.push(FlatNode {
            parent_idx: None,
            bus_name: root.bus_name().to_string(),
            path: root.object_path().to_string(),
            role: root.role(),
            name: root.name_or_empty(),
            automation_id: root.automation_id(),
            text: None,
            x: 0,
            y: 0,
            width: 0,
            height: 0,
            enabled: false,
        });
    }

    SnapshotArenaResult {
        arena: Arc::new(assemble_arena(arena, pid)),
        truncated,
        truncation_reason,
    }
}

fn assemble_arena(flat: Vec<FlatNode>, pid: u32) -> SnapshotArena {
    let mut nodes: Vec<ArenaNode> = flat
        .into_iter()
        .map(|f| ArenaNode {
            parent_idx: f.parent_idx,
            children: Vec::new(),
            bus_name: f.bus_name,
            path: f.path,
            role: f.role,
            name: f.name,
            automation_id: f.automation_id,
            text: f.text,
            x: f.x,
            y: f.y,
            width: f.width,
            height: f.height,
            enabled: f.enabled,
        })
        .collect();
    for idx in 1..nodes.len() {
        if let Some(pidx) = nodes[idx].parent_idx {
            if pidx < nodes.len() {
                nodes[pidx].children.push(idx);
            }
        }
    }
    SnapshotArena { pid, nodes }
}

fn assemble_element_node(arena: &SnapshotArena, idx: usize) -> ElementNode {
    let n = &arena.nodes[idx];
    let text = if n.text.as_deref() == Some(n.name.as_str()) {
        None
    } else {
        n.text.clone()
    };
    ElementNode {
        role: n.role.clone(),
        name: n.name.clone(),
        automation_id: n.automation_id.clone(),
        text,
        x: n.x,
        y: n.y,
        width: n.width,
        height: n.height,
        children: n
            .children
            .iter()
            .map(|&cidx| assemble_element_node(arena, cidx))
            .collect(),
    }
}

async fn read_wave_node(
    client: &A11yClient,
    item: QueuedRef,
    max_depth: usize,
    child_budget: usize,
    mode: WaveReadMode,
) -> Option<ReadWaveNode> {
    let proxy = client
        .accessible_proxy(&item.bus_name, &item.path)
        .await
        .ok()?;
    let role_enum = proxy.get_role().await.ok();

    let (raw_name, automation_id, states, ifaces) = match mode {
        WaveReadMode::Full => {
            let (n, id, st, ifc) = tokio::join!(
                proxy.name(),
                proxy.accessible_id(),
                proxy.get_state(),
                proxy.get_interfaces(),
            );
            (n, id, st, ifc.unwrap_or_default())
        }
        WaveReadMode::SelectorOnly => {
            let (n, id, st) = tokio::join!(
                proxy.name(),
                proxy.accessible_id(),
                proxy.get_state(),
            );
            (n, id, st, Default::default())
        }
    };

    let enabled = states
        .as_ref()
        .map(|s| s.contains(State::Enabled))
        .unwrap_or(false);

    let role = match role_enum {
        Some(r) => {
            let editable = r == Role::Text
                && states
                    .as_ref()
                    .map(|s| s.contains(State::Editable))
                    .unwrap_or(false);
            normalize_role(r, editable)
        }
        None => proxy.get_role_name().await.unwrap_or_default(),
    };

    let name = strip_placeholder_text(&raw_name.unwrap_or_default());
    let automation_id = automation_id.ok().filter(|s| !s.is_empty());

    let (bounds, text) = if mode == WaveReadMode::Full {
        let proxies_opt = if ifaces.contains(Interface::Component) || ifaces.contains(Interface::Text) {
            proxy.proxies().await.ok()
        } else {
            None
        };

        let bounds = if ifaces.contains(Interface::Component) {
            if let Some(ref proxies) = proxies_opt {
                if let Ok(comp) = proxies.component().await {
                    comp.get_extents(CoordType::Screen).await.unwrap_or((0, 0, 0, 0))
                } else {
                    (0, 0, 0, 0)
                }
            } else {
                (0, 0, 0, 0)
            }
        } else {
            (0, 0, 0, 0)
        };

        let text = if ifaces.contains(Interface::Text) {
            if let Some(ref proxies) = proxies_opt {
                if let Ok(tp) = proxies.text().await {
                    match tp.character_count().await {
                        Ok(len) if len > 0 => clean_optional_text(tp.get_text(0, len).await.ok()),
                        _ => None,
                    }
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

        (bounds, text)
    } else {
        ((0, 0, 0, 0), None)
    };

    let (mut children, mut children_truncated) = if item.depth < max_depth {
        client
            .children_refs_bounded_async(&item.bus_name, &item.path, child_budget)
            .await
            .unwrap_or((Vec::new(), false))
    } else {
        let has_any = proxy.child_count().await.unwrap_or(0) > 0;
        (Vec::new(), has_any)
    };

    // Preserve Qt/GTK popup overlay sibling exposure for top-level window frames.
    if item.depth == 0 && matches!(role_enum, Some(Role::Frame | Role::Window)) {
        if let Ok(parent_ref) = proxy.parent().await {
            if let Some((p_bus, p_path)) = crate::a11y::client::extract_ref(&parent_ref) {
                if let Ok((app_children, _)) = client
                    .children_refs_bounded_async(&p_bus, &p_path, 32)
                    .await
                {
                    if app_children.len() > 1 {
                        for (s_bus, s_path) in app_children {
                            if s_bus == item.bus_name && s_path == item.path {
                                continue;
                            }
                            if is_visible_overlay_async(client, &s_bus, &s_path).await {
                                if children.len() >= child_budget {
                                    children_truncated = true;
                                    break;
                                }
                                children.push((s_bus, s_path));
                            }
                        }
                    }
                }
            }
        }
    }

    Some(ReadWaveNode {
        parent_idx: item.parent_idx,
        depth: item.depth,
        bus_name: item.bus_name,
        path: item.path,
        role,
        name,
        automation_id,
        text,
        bounds,
        enabled,
        children,
        children_truncated,
    })
}

async fn is_visible_overlay_async(client: &A11yClient, bus: &str, path: &str) -> bool {
    let Ok(proxy) = client.accessible_proxy(bus, path).await else {
        return false;
    };
    let Ok(role) = proxy.get_role().await else {
        return false;
    };
    if matches!(role, Role::Frame | Role::Window | Role::Application) {
        return false;
    }
    let Ok(states) = proxy.get_state().await else {
        return false;
    };
    if !states.contains(State::Visible) || !states.contains(State::Showing) {
        return false;
    }
    let Ok(proxies) = proxy.proxies().await else {
        return false;
    };
    let Ok(comp) = proxies.component().await else {
        return false;
    };
    matches!(comp.get_extents(CoordType::Screen).await, Ok((_, _, w, h)) if w > 0 && h > 0)
}

/// Snapshot the full element tree of the window identified by `hwnd`.
pub fn snapshot_tree(hwnd: u64) -> Result<ElementNode> {
    let _ = A11yClient::shared()?;
    let win = find_matching_window(None, None, None, None, Some(hwnd))?;
    Ok(snapshot_tree_with_limits(&win, &SnapshotLimits::default()).root)
}

/// Build an AT-SPI2 element tree (or filtered selector matches) as JSON.
pub fn build_element_tree(
    pid: Option<u32>,
    title: Option<&str>,
    automation_id: Option<&str>,
    process: Option<&str>,
    hwnd: Option<u64>,
    max_results: usize,
    selector: Option<&str>,
) -> Result<serde_json::Value> {
    let desktop = Desktop::new();
    build_element_tree_with(
        &desktop,
        pid,
        title,
        automation_id,
        process,
        hwnd,
        max_results,
        selector,
    )
}

/// Same as [`build_element_tree`] but reuses an existing [`Desktop`] instance.
///
/// Uses bounded concurrent BFS traversal, aggregates secondary same-PID/process
/// windows when unscoped by `hwnd`/`title`, attaches truncation metadata when
/// budgets fire (without losing partial trees), and spills payloads past 32 KB
/// to a private temporary file.
#[allow(clippy::too_many_arguments)]
pub fn build_element_tree_with(
    desktop: &Desktop,
    pid: Option<u32>,
    title: Option<&str>,
    automation_id: Option<&str>,
    process: Option<&str>,
    hwnd: Option<u64>,
    max_results: usize,
    selector: Option<&str>,
) -> Result<serde_json::Value> {
    let windows = find_matching_windows_with(desktop, pid, title, automation_id, process, hwnd)?;
    let limits = SnapshotLimits::default();

    let payload = if let Some(sel_str) = selector {
        let path = ui_automata::SelectorPath::parse(sel_str)
            .map_err(|e| UiError::Internal(e.to_string()))?;
        let mut nodes: Vec<ElementNode> = Vec::new();
        for win in &windows {
            if nodes.len() >= max_results {
                break;
            }
            for matched in path.find_all(win) {
                if nodes.len() >= max_results {
                    break;
                }
                nodes.push(snapshot_tree_with_limits(&matched, &limits).root);
            }
        }
        serde_json::to_value(nodes)?
    } else {
        let mut iter = windows.into_iter();
        let primary = iter.next().expect("non-empty matched windows");
        let mut snap = snapshot_tree_with_limits(&primary, &limits);
        let mut remaining_nodes = limits.max_nodes.saturating_sub(snap.node_count);

        // Multi-root / multi-window aggregation for same PID/process without narrow title/hwnd.
        if hwnd.is_none() && title.is_none() && automation_id.is_none() {
            for extra_win in iter {
                if remaining_nodes < 16 {
                    snap.truncated = true;
                    snap.truncation_reason
                        .get_or_insert_with(|| "node budget exhausted across multi-window PID".into());
                    break;
                }
                let sub_limits = SnapshotLimits::clamped(
                    remaining_nodes,
                    limits.max_depth,
                    Duration::from_secs(3),
                );
                let sub_snap = snapshot_tree_with_limits(&extra_win, &sub_limits);
                remaining_nodes = remaining_nodes.saturating_sub(sub_snap.node_count);
                snap.truncated |= sub_snap.truncated;
                if snap.truncation_reason.is_none() {
                    snap.truncation_reason = sub_snap.truncation_reason;
                }
                snap.root.children.push(sub_snap.root);
            }
        }

        let mut json = serde_json::to_value(&snap.root)?;
        if snap.truncated {
            if let Some(obj) = json.as_object_mut() {
                obj.insert("truncated".to_string(), serde_json::Value::Bool(true));
                if let Some(reason) = snap.truncation_reason {
                    obj.insert(
                        "truncation_reason".to_string(),
                        serde_json::Value::String(reason),
                    );
                }
            }
        }
        json
    };

    Ok(spill_json_if_oversized(
        payload,
        INLINE_TREE_RESULT_BYTES,
        None,
    ))
}

/// Find all elements matching `selector` within windows matching the given filters.
pub fn find_elements(
    pid: Option<u32>,
    title: Option<&str>,
    automation_id: Option<&str>,
    process: Option<&str>,
    hwnd: Option<u64>,
    selector: &str,
    include_ancestors: bool,
    include_siblings: bool,
    max_results: usize,
) -> Result<Vec<ElementFindResult>> {
    let desktop = Desktop::new();
    find_elements_with(
        &desktop,
        pid,
        title,
        automation_id,
        process,
        hwnd,
        selector,
        include_ancestors,
        include_siblings,
        max_results,
    )
}

fn build_find_result_from_arena(
    arena: &SnapshotArena,
    idx: usize,
    win_pid: u32,
    include_ancestors: bool,
    include_siblings: bool,
) -> ElementFindResult {
    let n = &arena.nodes[idx];
    let name = (!n.name.is_empty()).then(|| n.name.clone());
    let value = n
        .text
        .as_deref()
        .map(strip_placeholder_text)
        .filter(|s| !s.is_empty());

    let ancestors = if include_ancestors {
        let mut anc = Vec::new();
        let mut cur_parent = n.parent_idx;
        while let Some(pidx) = cur_parent {
            let p = &arena.nodes[pidx];
            anc.push(AncestorInfo {
                name: (!p.name.is_empty()).then(|| p.name.clone()),
                role: p.role.clone(),
                automation_id: p.automation_id.clone(),
            });
            cur_parent = p.parent_idx;
        }
        anc.reverse();
        anc
    } else {
        vec![]
    };

    let siblings = if include_siblings {
        if let Some(pidx) = n.parent_idx {
            arena.nodes[pidx]
                .children
                .iter()
                .filter(|&&cidx| cidx != idx)
                .map(|&cidx| {
                    let child = &arena.nodes[cidx];
                    SiblingInfo {
                        name: (!child.name.is_empty()).then(|| child.name.clone()),
                        role: child.role.clone(),
                        value: child
                            .text
                            .as_deref()
                            .map(strip_placeholder_text)
                            .filter(|s| !s.is_empty()),
                        enabled: child.enabled,
                    }
                })
                .collect()
        } else {
            vec![]
        }
    } else {
        vec![]
    };

    ElementFindResult {
        name,
        role: n.role.clone(),
        value,
        enabled: n.enabled,
        bounds: Some((n.x, n.y, n.width, n.height)),
        automation_id: n.automation_id.clone(),
        pid: win_pid,
        ancestors,
        siblings,
    }
}

fn live_elem_from_arena(
    client: &Arc<A11yClient>,
    arena: &SnapshotArena,
    idx: usize,
) -> Option<LinuxElement> {
    let n = arena.nodes.get(idx)?;
    if n.bus_name.is_empty() || n.path.is_empty() {
        return None;
    }
    Some(LinuxElement::new(
        Arc::clone(client),
        n.bus_name.clone(),
        n.path.clone(),
    ))
}

/// Resolves the first descendant matching `selector` plus its step-parent via
/// concurrent wave-BFS (`SelectorOnly` mode) instead of synchronous D-Bus DFS.
pub fn find_descendant_with_parent_wave(
    root: &LinuxElement,
    selector: &ui_automata::SelectorPath,
) -> Option<(LinuxElement, Option<LinuxElement>)> {
    let arena_res = snapshot_arena_with_limits(
        root,
        &SnapshotLimits::clamped(
            FIND_SNAPSHOT_MAX_NODES,
            DEFAULT_SNAPSHOT_MAX_DEPTH,
            DEFAULT_SNAPSHOT_TIMEOUT,
        ),
        WaveReadMode::SelectorOnly,
    );
    let snap_root = SnapshotNodeRef {
        arena: Arc::clone(&arena_res.arena),
        idx: 0,
    };
    let (matched, step_parent) = selector.find_one_with_parent(&snap_root)?;
    let live_el = live_elem_from_arena(&root.client, &arena_res.arena, matched.idx)?;
    let live_parent =
        step_parent.and_then(|p| live_elem_from_arena(&root.client, &arena_res.arena, p.idx));
    Some((live_el, live_parent))
}

/// Resolves all descendants matching `selector` via concurrent wave-BFS (`SelectorOnly` mode).
pub fn find_all_descendants_wave(
    root: &LinuxElement,
    selector: &ui_automata::SelectorPath,
) -> Vec<LinuxElement> {
    let arena_res = snapshot_arena_with_limits(
        root,
        &SnapshotLimits::clamped(
            FIND_SNAPSHOT_MAX_NODES,
            DEFAULT_SNAPSHOT_MAX_DEPTH,
            DEFAULT_SNAPSHOT_TIMEOUT,
        ),
        WaveReadMode::SelectorOnly,
    );
    let snap_root = SnapshotNodeRef {
        arena: Arc::clone(&arena_res.arena),
        idx: 0,
    };
    selector
        .find_all(&snap_root)
        .into_iter()
        .filter_map(|m| live_elem_from_arena(&root.client, &arena_res.arena, m.idx))
        .collect()
}

/// Same as [`find_elements`] but reuses an existing [`Desktop`] instance.
///
/// Uses a single concurrent wave-BFS snapshot per matching window and resolves
/// selector matches, ancestors, and siblings in memory (zero follow-up D-Bus RPCs).
#[allow(clippy::too_many_arguments)]
pub fn find_elements_with(
    desktop: &Desktop,
    pid: Option<u32>,
    title: Option<&str>,
    automation_id: Option<&str>,
    process: Option<&str>,
    hwnd: Option<u64>,
    selector: &str,
    include_ancestors: bool,
    include_siblings: bool,
    max_results: usize,
) -> Result<Vec<ElementFindResult>> {
    if pid.is_none()
        && title.is_none()
        && automation_id.is_none()
        && process.is_none()
        && hwnd.is_none()
    {
        anyhow::bail!(
            "find_elements requires at least one of: pid, title, automation_id, process, hwnd"
        );
    }

    let path = ui_automata::SelectorPath::parse(selector)
        .map_err(|e| anyhow::anyhow!("bad selector: {e}"))?;

    let windows = desktop.application_windows()?;
    let windows_to_search: Vec<LinuxElement> = if let Some(h) = hwnd {
        let win = windows
            .into_iter()
            .find(|w| synthetic_hwnd(w.bus_name(), w.object_path()) == h)
            .ok_or_else(|| anyhow::anyhow!("window hwnd={h:#x} not found"))?;
        vec![win]
    } else {
        windows
            .into_iter()
            .filter(|w| matches_window_filters(w, pid, title, automation_id, process))
            .collect()
    };

    let limits = SnapshotLimits::clamped(
        FIND_SNAPSHOT_MAX_NODES,
        DEFAULT_SNAPSHOT_MAX_DEPTH,
        DEFAULT_SNAPSHOT_TIMEOUT,
    );
    let mut results = Vec::new();
    'outer: for win in &windows_to_search {
        let arena_res = snapshot_arena_with_limits(win, &limits, WaveReadMode::Full);
        let win_pid = arena_res.arena.pid;
        let snap_root = SnapshotNodeRef {
            arena: Arc::clone(&arena_res.arena),
            idx: 0,
        };
        for matched in path.find_all(&snap_root) {
            if results.len() >= max_results {
                break 'outer;
            }
            results.push(build_find_result_from_arena(
                &arena_res.arena,
                matched.idx,
                win_pid,
                include_ancestors,
                include_siblings,
            ));
        }
    }

    Ok(results)
}

/// Captures a high-budget non-spilled [`SnapshotTreeResult`] plus window metadata for
/// offline snapshot bundles (`automata explore capture-bundle`).
#[allow(clippy::too_many_arguments)]
pub fn capture_window_tree_snapshot(
    desktop: &Desktop,
    pid: Option<u32>,
    title: Option<&str>,
    automation_id: Option<&str>,
    process: Option<&str>,
    hwnd: Option<u64>,
    max_nodes: usize,
    max_depth: usize,
) -> Result<(crate::windows_info::WindowInfo, SnapshotTreeResult)> {
    let win = find_matching_window_with(
        desktop,
        pid,
        title,
        automation_id,
        process,
        hwnd,
    )?;
    let client = A11yClient::shared()?;
    let info = crate::windows_info::inspect_window_info(&win, &client);
    let limits = SnapshotLimits::clamped(max_nodes, max_depth, DEFAULT_SNAPSHOT_TIMEOUT);
    let snap = snapshot_tree_with_limits(&win, &limits);
    Ok((info, snap))
}

/// Evaluates a `ui_automata::SelectorPath` offline against a deserialized [`ElementNode`]
/// tree snapshot (`tree.json`), returning results in the same shape as `find_elements`.
pub fn query_element_snapshot(
    root: &ElementNode,
    selector: &str,
    max_results: usize,
) -> Result<Vec<ElementFindResult>> {
    let path = ui_automata::SelectorPath::parse(selector)
        .map_err(|e| UiError::Internal(format!("bad selector: {e}")))?;
    let arena = Arc::new(SnapshotArena::from_element_node(root));
    let snap_root = SnapshotNodeRef {
        arena: Arc::clone(&arena),
        idx: 0,
    };
    let mut results = Vec::new();
    for matched in path.find_all(&snap_root) {
        if results.len() >= max_results {
            break;
        }
        results.push(build_find_result_from_arena(&arena, matched.idx, 0, false, false));
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_limits_clamp_to_hard_ceilings() {
        let limits = SnapshotLimits::clamped(99_999, 999, Duration::from_secs(5));
        assert_eq!(limits.max_nodes, HARD_SNAPSHOT_MAX_NODES);
        assert_eq!(limits.max_depth, HARD_SNAPSHOT_MAX_DEPTH);
        let zero = SnapshotLimits::clamped(0, 0, Duration::from_secs(5));
        assert_eq!(zero.max_nodes, 1);
        assert_eq!(zero.max_depth, 1);
    }

    #[test]
    fn assemble_arena_preserves_hierarchy_sibling_order_and_ancestors() {
        let flat = vec![
            FlatNode {
                parent_idx: None,
                bus_name: ":1.1".into(),
                path: "/root".into(),
                role: "window".into(),
                name: "Main".into(),
                automation_id: None,
                text: None,
                x: 0,
                y: 0,
                width: 800,
                height: 600,
                enabled: true,
            },
            FlatNode {
                parent_idx: Some(0),
                bus_name: ":1.1".into(),
                path: "/c0".into(),
                role: "button".into(),
                name: "First".into(),
                automation_id: None,
                text: None,
                x: 10,
                y: 10,
                width: 50,
                height: 20,
                enabled: true,
            },
            FlatNode {
                parent_idx: Some(0),
                bus_name: ":1.1".into(),
                path: "/c1".into(),
                role: "panel".into(),
                name: "Container".into(),
                automation_id: None,
                text: None,
                x: 10,
                y: 40,
                width: 200,
                height: 100,
                enabled: true,
            },
            FlatNode {
                parent_idx: Some(2),
                bus_name: ":1.1".into(),
                path: "/c2".into(),
                role: "edit".into(),
                name: "Input".into(),
                automation_id: Some("input-1".into()),
                text: Some("hello".into()),
                x: 15,
                y: 45,
                width: 100,
                height: 24,
                enabled: true,
            },
        ];

        let arena = assemble_arena(flat, 42);
        let root = assemble_element_node(&arena, 0);
        assert_eq!(root.name, "Main");
        assert_eq!(root.children.len(), 2);
        assert_eq!(root.children[0].name, "First");
        assert_eq!(root.children[1].name, "Container");
        assert_eq!(root.children[1].children.len(), 1);
        assert_eq!(root.children[1].children[0].name, "Input");
        assert_eq!(
            root.children[1].children[0].automation_id.as_deref(),
            Some("input-1")
        );

        let found = build_find_result_from_arena(&arena, 3, 42, true, true);
        assert_eq!(found.pid, 42);
        assert_eq!(found.ancestors.len(), 2);
        assert_eq!(found.ancestors[0].name.as_deref(), Some("Main"));
        assert_eq!(found.ancestors[1].name.as_deref(), Some("Container"));
        assert!(found.siblings.is_empty());

        let sibling_check = build_find_result_from_arena(&arena, 1, 42, false, true);
        assert_eq!(sibling_check.siblings.len(), 1);
        assert_eq!(sibling_check.siblings[0].name.as_deref(), Some("Container"));
    }

    #[test]
    fn offline_snapshot_roundtrips_json_and_supports_parent_selectors() {
        let root = ElementNode {
            role: "window".into(),
            name: "通讯录管理".into(),
            automation_id: None,
            text: None,
            x: 920,
            y: 424,
            width: 720,
            height: 560,
            children: vec![
                ElementNode {
                    role: "list".into(),
                    name: String::new(),
                    automation_id: None,
                    text: None,
                    x: 1121,
                    y: 505,
                    width: 518,
                    height: 478,
                    children: vec![ElementNode {
                        role: "list item".into(),
                        name: "Ryze 大叔 ".into(),
                        automation_id: None,
                        text: None,
                        x: 1121,
                        y: 505,
                        width: 518,
                        height: 48,
                        children: vec![],
                    }],
                },
                ElementNode {
                    role: "list".into(),
                    name: String::new(),
                    automation_id: None,
                    text: None,
                    x: 921,
                    y: 465,
                    width: 200,
                    height: 518,
                    children: vec![ElementNode {
                        role: "list item".into(),
                        name: "全部(1)".into(),
                        automation_id: None,
                        text: None,
                        x: 921,
                        y: 465,
                        width: 200,
                        height: 40,
                        children: vec![],
                    }],
                },
            ],
        };

        let serialized = serde_json::to_string(&root).unwrap();
        let restored: ElementNode = serde_json::from_str(&serialized).unwrap();
        let hits = query_element_snapshot(
            &restored,
            ">> [role=list]:nth(0) >> [role='list item'][name~=Ryze]",
            10,
        )
        .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name.as_deref(), Some("Ryze 大叔 "));
        assert_eq!(hits[0].bounds, Some((1121, 505, 518, 48)));

        let parent_hits = query_element_snapshot(
            &restored,
            ">> [role='list item'][name~=Ryze]:parent",
            10,
        )
        .unwrap();
        assert_eq!(parent_hits.len(), 1);
        assert_eq!(parent_hits[0].role, "list");
        assert_eq!(parent_hits[0].bounds, Some((1121, 505, 518, 478)));
    }
}
