# Mission Control (`mc`)

A read-only birds-eye view over your collection of running coding-agent panes, with enough semantic context to make manual delegation frictionless — without doing any delegation itself.

Think Jarvisy: one screen that knows what every agent is doing, where each conversation is, which panes are blocked, what each project is, and which panes are waiting on you.

## Quick Start

```bash
# Build
cargo build

# One-shot status table
cargo run -- status

# Or: daemon + TUI
cargo run -- serve          # in one pane
cargo run -- tui             # in another pane
```

## Architecture

```
herdr JSON-RPC ──┐
pi .jsonl tail ──┤── Collectors ──→ Reducer ──→ State Store ──→ Event Emitter
cwd scan ────────┘                                              │
                                                    ┌───────────┼───────────┐
                                                    ▼           ▼           ▼
                                                 TUI        Web UI    Push clients
```

Three orthogonal data sources, one-way arrows, render-only backend. See `DESIGN.md` and `MISSION_CONTROL_PRD.md` for the full spec.

## Subcommands

| Command | Description |
|---|---|
| `mc status` | Print the "needs-you" lane — all panes sorted by attention |
| `mc serve` | Start the long-running daemon |
| `mc tui` | Launch the ratatui dashboard |

## Requirements

- [herdr](https://herdr.dev) — terminal workspace manager (most coding-agent panes run inside herdr)
- Rust 1.96+