# Mission Control — Architecture & Design Decisions

**Status:** Phase 3 complete — `mc status`, `mc serve`, `mc tui`, `mc web` implemented
**Last updated:** 2026-07-14

---

## Workspace Structure

```
mission-control/
├── Cargo.toml                    # workspace root
├── MISSION_CONTROL_PRD.md        # product requirements (the spec)
├── DESIGN.md                     # this file
├── mc-schema/                    # Phase 0: types, serde, schemars (depends on nothing)
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── raw_signals.rs        # HerdrPaneSnapshot, PiSignals, ProjectProfile
│       ├── pane_view.rs          # PaneView, Flags, Attention, Vitals, etc.
│       ├── project.rs            # ProjectView, ProjectKind, ArtifactHint
│       └── events.rs             # EventKind, EventEnvelope, PaneViewPatch, Totals
├── mc-core/                      # Phase 1-2: library (depends on mc-schema)
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── collector/
│       │   ├── mod.rs
│       │   ├── herdr.rs          # JSON-RPC poll to herdr
│       │   ├── pi.rs             # inotify tail of .jsonl files
│       │   └── project.rs        # notify on cwd mtime
│       ├── reducer.rs            # pure fn: signals → Vec<PaneView>
│       ├── state.rs              # Arc<Mutex<ring + seq>> mirroring herdr EventHub
│       ├── rules.rs              # Flag computation, config-driven thresholds
│       └── transport/
│           ├── mod.rs
│           └── unix_socket.rs    # Phase 2: JSON-RPC server over Unix socket
└── mc/                           # Phase 1-2: CLI binary (depends on mc-core)
    ├── Cargo.toml
    └── src/
        ├── main.rs               # CLI: mc status | mc serve | mc tui
        ├── status.rs             # Phase 1: inline collectors + reducer, prints table
        ├── daemon.rs             # Phase 2: long-running daemon
        └── tui.rs                # Phase 2: ratatui client
```

## Decided Architecture

### 1. herdr Socket Discovery

**Decision:** Inherit from the environment. herdr sets `HERDR_SOCKET_PATH` in every pane it manages:

```
HERDR_SOCKET_PATH=/home/anupam/.config/herdr/herdr.sock
HERDR_ENV=1
HERDR_PANE_ID=w655ed16fb64291:p7
HERDR_TAB_ID=w655ed16fb64291:t5
HERDR_WORKSPACE_ID=w655ed16fb64291
```

Since Mission Control runs inside a herdr pane (or at minimum on the same machine), the socket path is always available via `$HERDR_SOCKET_PATH`. Fallback: `~/.config/herdr/herdr.sock`.

`HERDR_ENV=1` is a presence test — mc can detect whether it's running inside herdr at all.

### 2. Config File

**Decision:** `~/.config/mc/config.toml`, mirroring herdr's own `~/.config/herdr/config.toml` pattern.

```toml
# ~/.config/mc/config.toml
runaway_threshold = 25       # tools_since_last_user >= this → runaway
idle_threshold_secs = 900    # last_activity older than this → idle_long
arc_turns = 5                # last N user turns in conversation arc
```

- Optional — all fields have defaults, `mc status` works with zero config
- `mc --config /path/to/mc.toml` override
- CLI flags override config values
- Reload on `SIGHUP` (per PRD R8)

### 3. Workspace Scope

**Decision:** Show all workspaces by default. Mission Control is a "birds-eye view over the collection of running coding-agent panes." `herdr pane list` already returns panes from all workspaces — we use that. `$HERDR_WORKSPACE_ID` tells the TUI which pane is "this" pane for cursor highlighting.

### 4. Phase Boundaries

**Phase 0 — Contract only.** Schema crate with types from PRD §6, §7, §8. Compiles, round-trips through `serde_json`, exports JSON Schema. No collectors, no transports.

**Phase 1 — Single binary `mc status`.** Inline herdr JSON-RPC client, pi jsonl tailer, project scanner. Reducer merges signals into `PaneView`s. Prints the "needs-you" lane as a sorted table.

**Phase 2 — Daemon + TUI.** Split into long-running daemon (`mc serve`) and TUI client (`mc tui`). Unix-socket JSON-RPC between them. Sub-200ms update latency.

### 5. Separation of Concerns (from PRD §5)

All 8 rules (R1–R8) stand. Key ones for implementation:

- **R1:** Collectors emit `RawSignals` types only. Reducer never imports herdr's schema or pi's jsonl layout.
- **R2:** Backend has zero mutating capability. No `pane send-text` or herdr mutation handles.
- **R3:** Clients never touch sources. TUI consumes `PaneView` events only.
- **R7:** Backend never blocks on a client. Event hub buffers; clients pull `events_after(seq)`.

### 6. State Store Pattern

Mirrors herdr's `EventHub` (verified in `herdr/src/api/event_hub.rs`):

```rust
struct StateStore {
    next_sequence: u64,
    events: Vec<(u64, EventEnvelope)>,  // bounded ring, MAX_EVENTS = 512
}

fn push(&self, event: EventEnvelope)
fn events_after(&self, sequence: u64) -> Vec<(u64, EventEnvelope)>
fn current_sequence(&self) -> u64
```

`Arc<Mutex<StateStore>>` behind the JSON-RPC transport. Same `events_after(seq)` poll pattern for clients.

## Implementation Summary

### Phase 0 ✅ — Schema crate (`mc-schema`)

Types from PRD §6–§8 compiled with `serde` + `schemars`. 11 round-trip + JSON Schema tests pass.

| Module | Key types |
|---|---|
| `raw_signals.rs` | `HerdrPaneSnapshot`, `PiSignals`, `ContentBlock`, `ConversationTree`, `MessageNode`, `LeafSummary` |
| `pane_view.rs` | `PaneView`, `Flags`, `Attention` (Ord), `Vitals`, `VitalsDelta`, `CurrentActivity`, `TurnSummary`, `Totals` |
| `events.rs` | `EventKind`, `EventEnvelope`, `PaneViewPatch`, `McRequest`/`McMethod` (6 RPC methods) |
| `project.rs` | `ProjectView`, `ProjectKind` (Rust/Node/Python/Mixed/Unknown), `ProjectProfile` |

### Phase 1 ✅ — `mc status` inline CLI

Single binary that inlines collectors + reducer and prints the needs-you table.

| File | Role |
|---|---|
| `mc-core/src/collector/herdr.rs` | JSON-RPC client → `$HERDR_SOCKET_PATH` → `pane.list` → `HerdrPaneSnapshot` |
| `mc-core/src/collector/pi.rs` | `.jsonl` parser, `parentId`-based conversation tree walker, error detector |
| `mc-core/src/collector/project.rs` | Filesystem scanner (Cargo.toml/package.json/pyproject.toml → `ProjectProfile`) |
| `mc-core/src/reducer.rs` | Pure function merging 3 signal maps → `Vec<PaneView>` + `Totals` + `Flags` |
| `mc-core/src/config.rs` | `~/.config/mc/config.toml` parser with defaults |
| `mc-core/src/state.rs` | `EventHub`-mirroring ring buffer with `events_after(seq)` |
| `mc/src/status.rs` | Terminal table rendering sorted by `attention` desc |

### Phase 2 ✅ — Daemon + TUI

| File | Role |
|---|---|
| `mc-core/src/transport/unix_socket.rs` | JSON-RPC server on `$XDG_RUNTIME_DIR/mc.sock`; 6 methods |
| `mc/src/daemon.rs` | `mc serve` — 1s poll loop, collect-reduce-emit |
| `mc/src/tui.rs` | `mc tui` — ratatui client polling `mc.snapshot` every 1s |

## Running

```bash
# Standalone status (no daemon needed)
mc status

# Start daemon (one pane)
mc serve

# Launch TUI (another pane)
mc tui
```

**TUI controls:** `q` or `Esc` to quit.

## Known Gaps

See `KNOWN_GAPS.md`. Currently one item: cost extraction from pi's `usage.cost` records (always shows `$0.00`).