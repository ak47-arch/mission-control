---
type: Reference
title: Mission Control — Quickstart
description: Mission Control (mc) quickstart guide, subcommands, architecture overview, configuration, and documentation map
tags: [quickstart, overview, subcommands, configuration, architecture]
---

# Mission Control — Quickstart

**Mission Control** (`mc`) is a read-only birds-eye view over your collection of running coding-agent panes. It collects signals from three orthogonal sources — the herdr terminal workspace manager, pi session logs, and filesystem project scans — reduces them into per-pane semantic views, and renders dashboards via TUI, web, or CLI table. It does **not** send commands to agents, spawn panes, or route messages.

- Repository: <https://github.com/anupam-sobti/mission-control>
- Version: `0.0.1` (Phases 0–3 complete)
- License: AGPL-3.0-or-later

## Quick Start

```bash
cargo build

# One-shot status table (no daemon needed)
cargo run -- status

# Daemon + TUI
cargo run -- serve          # in one pane
cargo run -- tui             # in another pane

# Web dashboard (requires `mc serve` running)
cargo run -- web
# → http://localhost:9876
```

## Subcommands

| Command | Description |
|---|---|
| `mc status` | Inline collectors + reducer; prints the "needs-you" lane sorted by attention |
| `mc serve` | Long-running daemon: polls herdr, tails pi sessions, reduces, serves JSON-RPC over Unix socket |
| `mc tui` | ratatui terminal dashboard; connects to the daemon, polls `mc.snapshot` every 1s |
| `mc web` | Axum HTTP + SSE bridge to the daemon; serves the browser dashboard at `:9876` |
| `mc diagnose` | Inspects session-to-pane mapping and orphaned sessions; connects to daemon and scans pi session directory |

## Requirements

- [herdr](https://herdr.dev) — terminal workspace manager (provides pane metadata via `$HERDR_SOCKET_PATH`)
- Rust 1.96+

## What It Does

Mission Control answers one core question: **"Which agent panes need my attention right now?"** It does this by:

1. **Collecting** raw signals from three independent sources (herdr JSON-RPC, pi `.jsonl` session logs, filesystem project scans)
2. **Reducing** them into `PaneView` structs — a canonical per-pane state containing identity, project context, conversation arc, current activity, vitals, and computed flags
3. **Emitting** events through a monotonic-sequence event store to TUI, web, and future push clients

The backend has **zero mutating capability** — it never calls `pane send-text`, `pane split`, or any herdr mutation.

## Documentation Map

| Section | Page | What it covers |
|---|---|---|
| Architecture | [architecture/overview.md](architecture/overview.md) | Data pipeline, three-source orthogonality, design decisions, phase history, config, transport |
| Domain Types | [domain/pane-view.md](domain/pane-view.md) | Schema crate: PaneView, Flags, Attention, events, raw signals, project types |

### Supporting Documents (in repo root)

- [`DESIGN.md`](../DESIGN.md) — architecture decisions, workspace layout, phase boundaries
- [`MISSION_CONTROL_PRD.md`](../MISSION_CONTROL_PRD.md) — full product requirements, data sources, rules R1–R8
- [`KNOWN_GAPS.md`](../KNOWN_GAPS.md) — tracked gaps (cost extraction now fixed; remaining gaps: tests, CI, rules.rs, arc walk)

## Workspace Crates

| Crate | Purpose | Depends on |
|---|---|---|
| `mc-schema` | All data types, serde, JSON Schema export. The "frozen keel." | Nothing |
| `mc-core` | Collectors, reducer, state store, config, Unix-socket transport | `mc-schema` |
| `mc` | CLI binary: `status`, `serve`, `tui`, `web` subcommands | `mc-core`, `mc-schema` |
| `mc-web/` | Static HTML/JS for the web dashboard (embedded via `include_str!`) | None |

## Key Concepts (Quick Reference)

- **PaneView** — The canonical per-pane state struct. Everything a client needs to understand.
- **Attention** — Five-level urgency enum (`None`, `Low`, `Medium`, `High`, `Critical`) that drives the "needs-you" lane sorting.
- **Flags** — Computed state: `is_blocked`, `is_runaway`, `awaiting_user_reply` (bools), `idle_long_secs` (`Option<u64>`), and `attention` (the 5-level urgency enum).
- **Three-source join** — herdr pane list (`HerdrPaneSnapshot`) → pi session (`PiSignals` via `agent_session_path`) + project scan (`ProjectProfile` via `cwd`).
- **StateStore** — `Arc<Mutex<ring + seq>>` mirroring herdr's `EventHub` pattern. Bounded to 512 events.

## Configuration

Config lives at `~/.config/mc/config.toml`:

```toml
runaway_threshold = 25       # tools since last user ≥ this → runaway
idle_threshold_secs = 900    # last activity older than this → idle_long
arc_turns = 5                # last N user turns in conversation arc
```

All fields optional; `mc status` works with zero config. Also supports `herdr_socket` override in config (defaults to `$HERDR_SOCKET_PATH`).

## Backlog

| Area | Source | Reason deferred |
|---|---|---|
| Tests beyond schema round-trips | `mc-schema/src/lib.rs` tests | Only serde round-trip tests exist. No reducer, collector, or transport tests yet |
| GitHub Actions CI | `/.github/workflows/` | Only `openwiki-update.yml` exists; no build/test workflow |
| `rules.rs` module | Referenced in `DESIGN.md` §1 | File does not exist; flag computation is inlined in `reducer.rs` |
| Conversation arc per-turn independent walk | `mc-core/src/reducer.rs:164` (`build_arc`) | TODO comment: currently marks all historical turns as "active"; should walk each subtree independently |

**Fixed since last update:**
- Cost extraction from pi usage records — `mc-core/src/collector/pi.rs` now deserializes `usage.cost.total`
- Session mapping for orphaned panes — `mc-core/src/collector/herdr.rs` has fallback scan for pi session files by cwd