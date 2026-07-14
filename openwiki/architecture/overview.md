# Architecture Overview

Mission Control is a read-only dashboard that collects signals from three independent sources, reduces them into a unified per-pane state, and presents the result through TUI, web, and one-shot CLI views.

## Data Pipeline

```
herdr Unix socket ──→ HerdrPaneSnapshot ──┐
pi .jsonl files  ──→ PiSignals        ──→ Reducer ──→ Vec<PaneView> + Totals
cwd filesystem   ──→ ProjectProfile   ──┘     │
                                               ├── status: print table
                                               ├── serve:  store + emit events → Unix socket
                                               │                ├── tui:  poll + render
                                               │                └── web:  poll + HTTP/SSE
```

## Three-Source Orthogonality

The system implements **R1** from the PRD: collectors emit internal-only signal types, the reducer never imports herdr's wire schema or pi's JSONL layout, and clients only consume `PaneView` / events.

### Collector 1: herdr (`mc-core/src/collector/herdr.rs`)

Connects to herdr's JSON-RPC Unix socket at `$HERDR_SOCKET_PATH`. Calls `pane.list` and maps herdr's wire schema into `HerdrPaneSnapshot` — capturing pane identity (workspace/tab/pane IDs), agent name, focus state, `cwd`, and the critical `agent_session_path` bridge field.

- **Poll frequency**: 1s in daemon mode
- **Error handling**: returns empty vec if herdr is unreachable

### Collector 2: pi sessions (`mc-core/src/collector/pi.rs`)

Reads pi `.jsonl` session files from each pane's `agent_session_path`. Parses the full session log and builds a conversation DAG from `parentId` chains. Computes:

- `deepest_leaf_since_last_user` — the leaf-state of the conversation after the most recent user message
- `tool_calls_since_last_user` / `error_since_last_user` — for runaway and blocked detection
- Cumulative totals: turns, tool calls, model usage, thinking level

Known quirk: pi uses `"toolCall"` (camelCase) content blocks, handled explicitly in the parser.

Cost extraction: Fixed. `RawMessage` now deserializes `usage` (with `cost.total`), and `build_signals` accumulates `total_cost_usd` and `cost_since_last_user`. See `KNOWN_GAPS.md` for history.

### Collector 3: project scan (`mc-core/src/collector/project.rs`)

Scans each pane's `cwd` filesystem. Detects project kind via marker files:

| Marker | Kind |
|---|---|
| `Cargo.toml` | Rust |
| `package.json` | Node |
| `pyproject.toml` / `setup.py` | Python |
| Multiple | Mixed |
| None | Unknown |

Extracts project name/purpose from `Cargo.toml` `[package]` description or `package.json` `description` field. Notes recently-modified artifact directories (`target/`, `dist/`, `node_modules/`, etc.).

## Reducer (`mc-core/src/reducer.rs`)

Pure function: `reduce(herdr_panes, pi_signals, projects, config) → (Vec<PaneView>, Totals)`

### Join Logic

1. Iterate herdr pane snapshots
2. Join with pi signals by `agent_session_path`
3. Join with project profiles by `cwd`
4. Compute flags → build `PaneView`

### Flag Computation (`compute_flags`)

| Condition | Attention | Flag |
|---|---|---|
| Blocked on tool error, no assistant text after | Critical | `is_blocked = true` |
| Tools since last user ≥ `runaway_threshold` (default 25) | High | `is_runaway = true` |
| Idle with unanswered assistant text, not focused | Medium | `awaiting_user_reply = true` |
| Working normally | Low | — |
| No activity / unknown | None | — |

Attention derives `Ord` → `Critical > High > Medium > Low > None`.

### Conversation Arc (`build_arc`)

Last N user turns (configurable via `arc_turns`, default 5). Each turn is truncated to 100 chars. Currently marks all historical turns as "active"; a TODO at line 164 of `reducer.rs` notes that each turn subtree should be walked independently.

## State Store (`mc-core/src/state.rs`)

Bounded ring buffer behind `Arc<Mutex<StateStore>>`:

- Stores `(seq, EventEnvelope)` pairs, max 512 entries
- Monotonic sequence counter mirrors herdr's `EventHub` pattern
- Clients pull via `events_after(seq)` — poll-based, not streaming

On each daemon poll cycle, the old and new `PaneView` sets are diffed. Events emitted:

| Event | Trigger |
|---|---|
| `PaneAdded` | New pane appears in herdr |
| `PaneRemoved` | Pane disappears |
| `PaneViewPatch` | Any field change on existing pane |
| `AttentionChanged` | Attention level changed |
| `TotalsChanged` | Cross-pane aggregate changed |

## Transport (`mc-core/src/transport/unix_socket.rs`)

JSON-RPC server over Unix domain socket at `$XDG_RUNTIME_DIR/mc.sock`. Methods:

| Method | Description |
|---|---|
| `mc.snapshot` | Full `Vec<PaneView>` state dump |
| `mc.pane.get` | Single pane by ID |
| `mc.needs_attention` | Filtered + sorted by attention (descending) |
| `mc.totals` | Cross-pane aggregates |
| `events.subscribe` | Events after `after_seq` with optional `kinds` filter |
| `events.current_sequence` | Latest monotonic sequence number |

## CLI Binary (`mc/src/`)

### `mc status` (`mc/src/status.rs`)
One-shot mode. Runs all three collectors inline, reduces, prints the "needs-you" lane sorted by attention. No daemon, no Unix socket.

### `mc serve` (`mc/src/daemon.rs`)
Long-running daemon. Polls collectors at 1s intervals, reduces, diffs against previous state, emits events into `StateStore`, serves JSON-RPC over Unix socket.

### `mc tui` (`mc/src/tui.rs`)
ratatui terminal dashboard. Connects to daemon socket, polls `mc.snapshot` every 1s. Renders:
- Header bar: pane counts (total/working/idle/blocked) + total cost
- "Needs you" list: panes sorted by attention, showing flags + last activity

### `mc web` (`mc/src/web.rs`)
Axum HTTP + SSE bridge. Serves the static dashboard at `mc-web/index.html`, provides JSON API endpoints mirroring the daemon's RPC methods, and maintains an SSE stream for live updates on port 9876.

## Configuration (`mc-core/src/config.rs`)

Default config path: `~/.config/mc/config.toml`. All fields optional; `mc status` works with zero config.

```toml
runaway_threshold = 25       # tools since last user ≥ this → runaway
idle_threshold_secs = 900    # last activity older than this → idle_long
arc_turns = 5                # last N user turns in conversation arc
```

Also supports a `herdr_socket` override in config (defaults to `$HERDR_SOCKET_PATH`).

## Phase History

| Phase | Commits | Deliverables |
|---|---|---|
| 0-2 | `44a650e` | Schema crate, collectors, reducer, state store, config, transport, TUI, daemon, status |
| 3 | `dba6517` | Web dashboard: `mc-web/index.html`, `mc/src/web.rs`, Axum SSE bridge |

## Design Decisions

1. **Read-only by construction** — the backend never calls herdr mutation methods (`pane send-text`, `pane split`, etc.). This constraint is architectural, not enforced by permissions.
2. **Three-source orthogonality (R1-R3)** — collectors and reducer have internal types (`raw_signals.rs`), clients only see public types (`pane_view.rs`). The reducer never imports herdr or pi schemas.
3. **herdr-like event hub (R7)** — `StateStore` mirrors herdr's `EventHub` with ring buffer + monotonic sequence numbers. Clients poll `events_after(seq)`.
4. **Frozen keel** — `mc-schema` depends on nothing except `serde`, `schemars`, `chrono`, and `uuid`. It has zero knowledge of collectors, reducers, or transports.
5. **Web as static asset** — `mc-web/index.html` is a single vanilla HTML file baked into the binary via `include_str!`. No bundler, no framework.