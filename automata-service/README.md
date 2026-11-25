# automata-service

Unified session-aware `AutomationService` crate providing ergonomic Rust APIs, a typed `Command` / `Event` protocol, a local `0700/0600` Unix Domain Socket daemon, workflow recording, and the `automata` CLI/REPL binary.

## Capabilities

- **Rust API (`AutomationService`)**:
  - Core entrypoints: `AutomationService::open(ServiceConfig)`, `call(Command)`, `call_async(Command)`, `submit(Command)` (returns cancellable `CommandHandle`), `events()` (broadcast stream of `Event`), `info()`.
  - Fluent facades (`automata_service::api`):
    - `svc.session()` — service info, runtime capabilities, reset `ShadowDom`, start/status/stop workflow recording
    - `svc.explore()` — list windows, focused window, scoped tree, `find_elements`, offline `capture_bundle`, snapshot query (`query_snapshot`), multi-step fault-tolerant `batch` probes
    - `svc.act()` — `launch`, `invoke`, `set_value`, `click`, `close_window`, `run_steps`
    - `svc.vision()` — `window_layout`, `capture`
    - `svc.workflow()` — `list`, `lint_yaml`, `run_file`, `run_inline`, `status`, `cancel`
    - `svc.input()` / `svc.clipboard()` / `svc.system()` / `svc.file()` — direct input, clipboard, ping, and file helpers
- **Three-Lane Scheduler**:
  - Dedicated `UiActor` OS thread owning a persistent `Desktop` & `ShadowDom` instance;
  - Concurrent IO pool for non-blocking tasks;
  - Cancellable workflow runner.
- **Local UDS Daemon (`automata daemon`)**:
  - Keeps `Desktop`, `ShadowDom`, and launched GUI sessions alive across short-lived CLI calls.
  - Supports `systemd --user` installation via `automata daemon systemd-unit --install --enable`.

## CLI Usage

```sh
automata daemon start|status|stop|systemd-unit
automata explore windows|focused|tree|find|capture-bundle|batch
automata act launch|focus|close|invoke|set-value|click|steps|move-cursor
automata vision window-layout|screen-layout|capture
automata workflow list|lint|run|status|cancel
automata record start|status|stop
automata system ping|processes|capabilities|doctor
automata repl
```
