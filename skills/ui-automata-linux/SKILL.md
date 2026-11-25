---
name: ui-automata-linux
description: Operate live Linux desktop applications (X11, XWayland, Wayland) and Chromium/Edge browsers using CLI-first `/home/ivrty/.ui-automata/automata` (with persistent `0600` UDS/`systemd --user` daemon, offline `SnapshotBundle` exploration, `explore batch`, and `record`), and author, lint, and execute declarative YAML UI workflows. Activate when asked to inspect/control Linux GUI apps, automate desktop/browser UI tasks, capture/OCR screens, or write `.yml` UI workflows.
---

# UI Automata Linux Skill (CLI-Only)

Operate a live Linux desktop through the unified `AutomationService` CLI (`/home/ivrty/.ui-automata/automata --json`):
- Connects to a shared persistent Unix domain socket daemon (`automata daemon start` / `systemd --user`), preserving `Desktop`, `ShadowDom`, `Recorder`, and logged-in apps across turns without per-call JSON schema overhead.
- Supports **single-pass offline `SnapshotBundle` capture** (`explore capture-bundle` + `explore find --from-snapshot`), multi-step fault-tolerant probes (`explore batch`), interactive recording (`record start/stop`), and deterministic YAML workflow execution (`workflow run`).

Always pick the highest-reliability automation layer available, inspect before acting, and verify UI state changes after acting.

---

## 1. CLI Initialization & Standard Operating Loop

> Global CLI flags (valid on all subcommands): `--json`, `--socket <PATH>`, `--no-daemon`, `--workflows-dir <DIR>`.  
> Window target flags (`explore` / `act` / `record start`): `--hwnd <0x...>` (fastest), `--process <NAME>`, `--pid <PID>`, `--title <TEXT>`, `--selector <SEL>`.

1. **Ensure Persistent Daemon Session**:
   - Check `/home/ivrty/.ui-automata/automata --json daemon status`.
   - If not running, start it once (`automata daemon start`, or install `systemd --user` persistence via `automata daemon systemd-unit --install --enable`) so `ShadowDom`, `Recorder`, and launched windows persist across turns without killing/re-launching logged-in apps.
   - Check runtime readiness and active backend via `automata --json system capabilities` (and `automata system doctor --profile auto` if display/input is misconfigured).
2. **Step 1 — Acquire Window (`act launch` / `explore windows`)**:
   - Attach cleanly or launch: `automata --json explore windows` (or `automata --json act launch <TARGET> --wait new_window --timeout-ms 30000`) to capture `hwnd` (`"0x..."`), `pid`, `process`, and `bounds`.
   - If a validated workflow already exists (`automata --json workflow list`), prefer running it directly.
3. **Step 2 — Single-Pass Snapshot & Offline Iteration (Preferred)**:
   - Instead of polling the live AT-SPI2 tree repeatedly, capture a complete bundle once:
     `automata --json explore capture-bundle --hwnd <HWND> --ocr --out /tmp/ui_snap`
     (writes window metadata, `tree.json` up to `--max-nodes 5000` / `--max-depth 32`, PNG screenshot, and `ocr.json`).
   - Query selectors zero-cost offline against that snapshot as many times as needed:
     `automata --json explore find --from-snapshot /tmp/ui_snap --query ">> [role=button][name~=Save]"`
   - For live filtered checks use `automata --json explore find --hwnd <HWND> --query "<SEL>"` or scoped `automata --json explore tree --hwnd <HWND> --selector "<SEL>" --max-results 10`. **Never** dump unfiltered live trees without limits.
4. **Step 3 — Multi-Step Batch Probes**:
   - Probe multi-branch dialogs/menus without closing apps between steps using `automata --json explore batch --hwnd <HWND> --file probes.json --bundle-dir /tmp/ui_probes`:
     ```json
     [
       { "name": "open_menu", "step": { "intent": "open menu", "action": { "type": "Invoke", "scope": "window", "selector": ">> [role=button][name=Menu]" }, "expect": { "type": "Always" } }, "settle_ms": 300, "capture_tree": true, "capture_screenshot": true, "capture_ocr": false, "query": ">> [role=dialog]" }
     ]
     ```
     (`BatchProbeStep` continues on step failure unless `--stop-on-error` is passed.)
5. **Step 4 — Prototype Actions or Record (`act` / `record`)**:
   - Execute single actions (`act invoke`, `act set-value`, `act click`) or JSON step arrays (`act steps --hwnd <HWND> --file steps.json`) using `expect` assertions (100 ms polling — never hardcode sleeps).
   - Or capture live exploration into validated YAML: `automata record start <NAME> --hwnd <HWND>` -> run `act ...` steps -> `automata record stop --out workflows/linux/<name>.yml`.
6. **Step 5 — Verify Visual/Tree State**:
   - Re-check with `explore find`, `explore windows`, or `vision window-layout --hwnd <HWND>` — exit code `0` / `"ok": true` is not proof the UI transitioned.
7. **Step 6 — Promote to YAML Workflow**:
   - Validate with `automata --json workflow lint <FILE>` and execute with `automata --json workflow run <FILE>`.

---

## 2. Layered Strategy & CLI Capability Reference

**Priority Order**: **AT-SPI2 UI Tree (`explore` + `act`)** → **Browser CDP (`automata-browser` / `Browser*` steps)** → **OCR Vision (`vision` / `Ocr*` steps)** → **Raw Input (`Pointer*` / `PressKey` / `act move-cursor`)**.

| Capability | CLI Command (`automata --json ...`) | Notes |
| --- | --- | --- |
| Daemon / Readiness | `daemon start\|status\|stop\|systemd-unit`, `system ping\|processes\|capabilities\|doctor` | Persistent daemon binds per-user `0700/0600` UDS; pass `--no-daemon` for one-shot in-process execution. |
| Window / Tree / Snapshot | `explore windows\|focused\|tree\|find\|capture-bundle\|batch` | `capture-bundle` writes full offline `SnapshotBundle`; `find --from-snapshot` queries locally without live D-Bus traffic. |
| Vision / OCR / PNG | `vision window-layout --hwnd <H>`, `vision screen-layout`, `vision capture [--hwnd <H>] [--path <PNG>]` | Supports `ort-ppocr`, `tesseract`, or optional `omniparser` (`AUTOMATA_OMNIPARSER_URL`); prefer `window-layout` over `screen-layout`. |
| Launch & Window Ops | `act launch <TARGET> [--wait new_pid\|new_window\|match_any] [--timeout-ms 30000]`, `act focus\|close` | Auto-injects `QT_ACCESSIBILITY=1`, `QT_LINUX_ACCESSIBILITY_ALWAYS_ON=1`, `NO_AT_BRIDGE=0`. |
| Semantic & Step Actions | `act invoke --elem <S>`, `act set-value --elem <S> --value <V>`, `act click --elem <S>`, `act steps --file <JSON>`, `act move-cursor --x <X> --y <Y>` | Mounts Root anchor `"window"`; reuses `ShadowDom` across calls while window matches. |
| Workflows & Recording | `workflow list\|lint <F>\|run <F>\|status\|cancel`, `record start\|status\|stop` | `lint` returns structured diagnostics (`line`, `col`, `path`, `message`); `record stop` exports validated YAML. |
| Browser CDP / Standalone | `automata-browser tabs\|open\|navigate\|eval\|dom\|screenshot`, or `BrowserNavigate` / `BrowserEval` / `Pointer*` inside `act steps` & `.yml` | Use `desktop`/`act` for browser chrome UI (address bar, downloads dialog) and CDP for page DOM. |

---

## 3. Linux-Specific Behavioral Rules

1. **Wayland vs X11/XWayland**:
   - **Native Wayland clients (e.g. GTK4)**: AT-SPI2 `bounds()` frequently reports window-relative `(0, 0)` origin, making screen-coordinate `Click` miss. **Strongly prefer AT-SPI2 semantic actions (`Invoke`, `SetValue`, `Focus`, `PressKey`)** over `Click` / `ClickAt`.
   - **X11 / XWayland clients** (e.g. Electron, Chromium, Qt-XCB / `QT_QPA_PLATFORM=xcb`): support full screen coordinates and fast zero-flash in-memory `x11rb` capture.
   - **`ydotool` on Wayland**: `PointerScroll` / `PointerDrag` / pointer actions handle GNOME fractional/2x absolute scaling compensation and discrete `+-1` scroll ticks automatically; requires `ydotoold` running and `/dev/uinput` access.
2. **Qt & GTK Overlay Nodes / Custom Widgets**:
   - Popups, dropdowns, and modals often mount as siblings under the `Application` node rather than children of the main frame. `automata-linux` exposes visible overlay siblings under the window subtree — query them via `>> [role=menu] >> [role='menu item']` or `>> [role=dialog]`.
   - Hidden/offscreen Qt `QStackedLayout` pages can leave stale `0x0` placeholders (`ElementFound` true even when hidden): always prefer `ElementVisible` or inspect `visibility`/`bounds` rather than `ElementFound` alone when switching tabs/pages.
   - Some custom Qt widgets expose `[role=button]` without an AT-SPI2 `Action` interface (`Invoke` fails with no action interface) or trigger reliably only via `Click`, `ClickAt`, or `PressKey` (`{ENTER}` after `Focus`). Prefer `Invoke` -> fallback to `Click` / `PressKey` / `OcrClick`.
3. **HWND Staleness & Reuse**:
   - Synthetic HWNDs (`"0x..."`) derive from AT-SPI2 `(bus_name, object_path)`. Closing/recreating windows invalidates handles — re-run `automata explore windows`.
   - In YAML workflows where the app is already open and logged in (e.g. WeChat), configure `launch.reuse_existing: true` and `launch.env` (e.g. `QT_QPA_PLATFORM: xcb`) to attach without respawning.
4. **AppArmor & Snap Applications (Ubuntu)**:
   - On Ubuntu hosts where tools run under named AppArmor profiles (`ai.opencode (unconfined)`, etc.), Snap packages (`snap.firefox.firefox`, etc.) permit AT-SPI2 D-Bus traffic only from literal `peer=(label=unconfined)`.
   - `automata` skips `AccessDenied` Snap peers cleanly during enumeration; to automate Snap UI itself, prefix execution with `aa-exec -p unconfined -- automata ...`.
5. **Coexistence with Host Tools**:
   - Use native coding agent tools (`read_file`, `write_file`, `run_command`) for codebase files and reserve `automata` CLI (`explore`, `act`, `vision`, `workflow`, `record`) for desktop UI inspection and automation.

---

## 4. Selector Syntax & Normalized Linux Roles

### Combinators & Attribute Operators

- `>>` : Descendant (`scope` root down through arbitrary depth)
- `>` : Direct child
- `[attr=val]` : Exact match
- `[attr^=val]` / `[attr$=val]` / `[attr~=val]` : Prefix / suffix / substring contains
- `[attr=a|b]` : Alternation (`button|menu item`)
- `:nth(N)` : 0-based Nth match (`*` wildcard counts all children)
- `:parent` : Step back to matched node's parent

Quote values containing spaces or colons: `[role='menu item'][name='Save As...']`.

### Normalized Role Names (`normalize_role`)

Use normalized role strings in selectors (not raw AT-SPI2 enum names like `PushButton`):

- Windows / containers: `window`, `dialog`, `pane`, `menu`, `tab`, `tool tip`
- Controls: `button`, `edit`, `text`, `menu bar`, `menu item`, `list item`, `tree item`, `check box`, `radio button`, `combo box`, `tab item`, `document`

---

## 5. Step & Workflow Specification (`act steps` / `.yml`)

### Step Structure

```json
{
  "intent": "click Save button",
  "precondition": { "type": "ElementVisible", "scope": "window", "selector": ">> [role=button][name=Save]" },
  "action": { "type": "Invoke", "scope": "window", "selector": ">> [role=button][name=Save]" },
  "fallback": { "type": "OcrClick", "scope": "window", "pattern": { "exact": "Save" } },
  "expect": { "type": "DialogAbsent", "scope": "window" },
  "timeout": "8s",
  "on_failure": "abort",
  "on_success": "continue"
}
```

- `on_failure`: `"abort"` (default) \| `"continue"`
- `on_success`: `"continue"` (default) \| `"return_phase"`
- Variables: `{param.key}` (YAML params), `{output.key}` / `{local.key}` (runtime values populated by `Extract` / `OcrExtract` / `WriteOutput`, also usable inside `fallback`), `{env.VAR}`.

### Supported `Action` Types (Verbatim Engine Spec)

- **AT-SPI2 Element Actions** (require `scope` + `selector`):
  `Click`, `DoubleClick`, `Hover`, `ScrollIntoView`, `ClickAt`, `TypeText`, `Focus`, `Invoke`, `SetValue`, `Extract`
- **Window / Dialog Scope Actions** (require `scope`):
  `ActivateWindow`, `MinimizeWindow`, `CloseWindow`, `DismissDialog`
- **Foreground / Keyboard**:
  `PressKey` (optional `scope` + `selector`, or global window/desktop key), `ClickForegroundButton`, `ClickForeground`
- **Orthogonal Vision OCR & Screenshot**:
  `OcrClick`, `OcrHover`, `OcrTypeText`, `OcrExtract`, `Screenshot` (support optional `scope`+`selector` crop, `region`, `min_confidence`, `nth`, `offset_x`/`offset_y`)
- **Tier-4 Raw Pointer**:
  `PointerScroll`, `PointerDrag`
- **Browser CDP**:
  `BrowserNavigate`, `BrowserEval`
- **Control / Data / OS**:
  `NoOp`, `Sleep`, `WriteOutput`, `Exec`, `Eval`, `MoveFile`

### Supported `Condition` Types (Verbatim Engine Spec)

- **UI Tree**: `ElementFound`, `ElementEnabled`, `ElementVisible`, `ElementHasText`, `ElementHasChildren`
- **Window & Process**: `WindowWithAttribute`, `ProcessRunning`, `WindowClosed`, `WindowWithState`, `DialogPresent`, `DialogAbsent`, `ForegroundIsDialog`
- **OCR**: `OcrTextFound` (throttled ~400 ms per `(hwnd, region)`, cleared after each action)
- **Browser CDP**: `TabWithAttribute`, `TabWithState`
- **System / Combinators**: `FileExists`, `Always`, `ExecSucceeded`, `EvalCondition`, `AllOf`, `AnyOf`, `Not`

### Minimal Valid YAML Workflow Template

```yaml
name: example_linux_flow
params:
  - name: query
    default: "hello"
defaults:
  timeout: 10s
  retry:
    fixed: { count: 1, delay: 300ms }
launch:
  exe: my-app
  wait: new_window
  timeout: 15s
  reuse_existing: true
  env:
    QT_QPA_PLATFORM: xcb
anchors:
  app_win: { type: Root, process: my-app, selector: "*" }
  search_box: { type: Stable, parent: app_win, selector: ">> [role=edit]" }
phases:
  - name: search
    mount: [app_win, search_box]
    unmount: [search_box]
    steps:
      - intent: enter query
        action: { type: SetValue, scope: search_box, selector: "*", value: "{param.query}" }
        expect: { type: ElementHasText, scope: search_box, selector: "*", pattern: { contains: "{param.query}" } }
```

Always validate authored YAML with `automata --json workflow lint <FILE>` before executing.

---

## 6. Safety & User Communication

- State your intent plainly before triggering visible GUI actions, external downloads, or process kills.
- Ask before proceeding when actions are destructive (deleting data/contacts/files, sending messages, modifying system settings) or when multiple matching windows create ambiguity.
- Report observed outcomes concisely (window state, extracted outputs, or saved screenshot/snapshot paths) rather than assuming success.
