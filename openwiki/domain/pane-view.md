---
type: Reference
title: Domain Types — mc-schema
description: The mc-schema crate types including PaneView, Flags, Attention, events, raw signals, and project types
tags: [domain, schema, types, pane-view, flags, attention, events, project]
---

# Domain Types — `mc-schema`

The `mc-schema` crate is the "frozen keel" of Mission Control. It contains all data types, depends on nothing except `serde`, `schemars`, `chrono`, and `uuid`, and is consumed by both the backend (`mc-core`) and all clients (`mc tui`, `mc web`).

## Module Map

| File | Purpose | Visibility |
|---|---|---|
| `pane_view.rs` | Public per-pane state: `PaneView`, `Flags`, `Attention`, vitals, activity | Public |
| `events.rs` | Wire protocol: events, JSON-RPC request/response types | Public |
| `raw_signals.rs` | Internal types emitted by collectors: `HerdrPaneSnapshot`, `PiSignals`, etc. | Internal (R1) |
| `project.rs` | Project metadata: `ProjectView`, `ProjectKind`, `ProjectProfile` | Mixed |

---

## `PaneView` — The Centerpiece

Source: `mc-schema/src/pane_view.rs`

The canonical per-pane state. Every client consumes this.

| Group | Field | Type |
|---|---|---|
| Identity | `schema_version` | `u32` |
| | `pane_id` | `String` |
| | `workspace_id` | `String` |
| | `tab_id` | `String` |
| | `workspace_name` | `Option<String>` — human-readable label from herdr |
| | `tab_name` | `Option<String>` — human-readable label from herdr |
| | `updated_at` | `DateTime<Utc>` |
| Location | `agent` | `String` (e.g. `"pi"`) |
| | `agent_status` | `AgentStatus` |
| | `focused` | `bool` |
| | `cwd` | `Option<PathBuf>` — working directory of the pane |
| | `session_id` | `Option<Uuid>` |
| | `session_path` | `Option<PathBuf>` |
| Project | `project` | `Option<ProjectView>` |
| Conversation | `last_user_message` | `Option<String>` (truncated to 100 chars) |
| | `arc` | `Vec<TurnSummary>` |
| | `current` | `CurrentActivity` |
| Vitals | `vitals` | `Vitals` |
| | `vitals_since_last_user` | `VitalsDelta` |
| Flags | `flags` | `Flags` |

---

## `Flags` & `Attention` — The "Needs-You" Kernel

Source: `mc-schema/src/pane_view.rs`

### `Flags`

| Field | Type | Meaning |
|---|---|---|
| `attention` | `Attention` | 5-level urgency enum |
| `is_runaway` | `bool` | Tools since last user ≥ threshold |
| `is_blocked` | `bool` | Tool error with no assistant text after |
| `awaiting_user_reply` | `bool` | Idle with unanswered assistant text |
| `idle_long_secs` | `Option<u64>` | Seconds since last activity (if exceeding idle threshold) |

### `Attention`

Derives `Ord`: **Critical > High > Medium > Low > None**.

| Variant | Condition | Meaning |
|---|---|---|
| `Critical` | Blocked on tool error | Needs immediate attention |
| `High` | Runaway (excessive tool calls) | May need intervention |
| `Medium` | Awaiting user reply | Has unanswered question |
| `Low` | Working normally | All good |
| `None` | No activity / unknown | Nothing to report |

---

## Activity Tracking

### `CurrentActivity`

| Field | Type |
|---|---|
| `kind` | `ActivityKind` |
| `tool_name` | `Option<String>` |
| `snippet` | `String` |
| `started_at` | `DateTime<Utc>` |

### `ActivityKind`

| Variant | Meaning |
|---|---|
| `Thinking` | Agent is reasoning |
| `ToolCall` | Agent is calling a tool |
| `ToolResult` | Tool result received |
| `UserPending` | Waiting for user input |

### `AgentStatus`

| Variant |
|---|
| `Idle` |
| `Working` |
| `Blocked` |
| `Done` |
| `Unknown` |

---

## Conversation Arc

### `TurnSummary`

| Field | Type |
|---|---|
| `question` | `String` (truncated to 100 chars) |
| `asked_at` | `DateTime<Utc>` |
| `ended` | `TurnEnd` |

### `TurnEnd`

| Variant | Meaning |
|---|---|
| `Answered` | Agent finished responding |
| `Errored` | Agent hit an error |
| `Active` | Turn still in progress |

---

## Vitals

### `Vitals` (totals)

| Field | Type |
|---|---|
| `total_turns` | `u64` |
| `total_tool_calls` | `u64` |
| `total_cost_usd` | `f64` |
| `model` | `Vec<String>` |
| `thinking_level` | `Option<String>` |
| `session_age` | `Option<Duration>` |

### `VitalsDelta` (since last user)

| Field | Type |
|---|---|
| `tool_calls` | `u64` |
| `cost_usd` | `f64` |
| `errors` | `u64` |

### `Totals` (cross-pane)

| Field | Type |
|---|---|
| `pane_count` | `usize` |
| `working_count` | `usize` |
| `idle_count` | `usize` |
| `blocked_count` | `usize` |
| `total_cost` | `f64` |
| `total_tool_calls` | `u64` |

---

## Wire Protocol

Source: `mc-schema/src/events.rs`

### `EventKind`

| Variant | Payload | Emitted when |
|---|---|---|
| `PaneAdded` | Full `PaneView` | New pane appears |
| `PaneRemoved` | `pane_id: String` | Pane disappears |
| `PaneViewPatch` | `PaneViewPatch` (all fields optional) | Any field change |
| `AttentionChanged` | `{pane_id, from, to}` | Attention level changed |
| `TotalsChanged` | `Totals` | Cross-pane aggregate changed |

### `EventEnvelope`

Wraps every event with `protocol_version`, `sequence` (monotonic u64), `timestamp`, and the `EventKind` payload.

### JSON-RPC Methods (`McMethod`)

| Method | Request | Response |
|---|---|---|
| `mc.snapshot` | `McRequest` | `SnapshotResponse { panes, totals }` |
| `mc.pane.get` | `McRequest { pane_id }` | `PaneGetResponse` |
| `mc.needs_attention` | `McRequest` | `NeedsAttentionResponse { panes }` |
| `mc.totals` | `McRequest` | `TotalsResponse { totals }` |
| `events.subscribe` | `McRequest { after_seq, kinds }` | `EventsResponse { events }` |
| `events.current_sequence` | `McRequest` | `CurrentSequenceResponse { sequence }` |

---

## Internal Types (Collector → Reducer)

Source: `mc-schema/src/raw_signals.rs`

These types implement **R1**: collectors emit them, the reducer consumes them, and clients never see them.

### `HerdrPaneSnapshot`

Point-in-time snapshot from `herdr pane list`. Key fields: `workspace_id`, `tab_id`, `pane_id`, `agent`, `agent_status`, `focused`, `cwd`, `agent_session_path` (bridge to pi signals), `custom_status`, `captured_at`.

### `PiSignals`

Cumulative + live-derived view of a pi session `.jsonl` file. Built from `ConversationTree` (a DAG of `MessageNode` structs linked by `parentId`). Key fields: `session_id`, `session_path`, `started_at`, `model: Vec<ModelId>`, `thinking_level`, `total_turns`, `total_tool_calls`, `total_cost_usd`, `last_user_message`, `deepest_leaf_since_last_user: LeafSummary`, `tool_calls_since_last_user`, `error_since_last_user: Option<ErrorSummary>`, `last_activity_at`.

### Supporting Types

- `ContentBlock` — `Text`, `Thinking`, `ToolCall` (note: pi uses camelCase `"toolCall"`), `ToolResult`
- `MessageRole` — `User`, `Assistant`, `ToolResult`
- `LeafSummary` / `LeafKind` — leaf-state after last user message
- `ErrorSummary` — tool error details
- `ModelId` — `{ provider, model_id }`
- `ThinkingLevel` — `Xhigh`, `High`, `Medium`, `Low`

---

## Project Types

Source: `mc-schema/src/project.rs`

### `ProjectView` (public)

| Field | Type |
|---|---|
| `kind` | `ProjectKind` |
| `name` | `String` |
| `purpose` | `Option<String>` |
| `stack_summary` | `Vec<String>` |
| `recent_artifacts` | `Vec<ArtifactHint>` |
| `scanned_at` | `DateTime<Utc>` |

### `ProjectKind`

| Variant | Markers |
|---|---|
| `Rust` | `Cargo.toml` |
| `Node` | `package.json` |
| `Python` | `pyproject.toml` / `setup.py` |
| `Mixed` | Multiple detected |
| `Unknown` | No markers found |

### `ArtifactHint`

Hints about recently-modified output directories: `path`, `kind` (target, dist, node_modules, etc.), `mtime`.

### `ProjectProfile` (internal)

Extends `ProjectView`-like fields with `cwd_mtime` for cache invalidation. Converted to `ProjectView` via `.to_view()`.

---

## Architecture Rules

| Rule | Effect |
|---|---|
| **R1** | `raw_signals.rs` types never leak to clients |
| **R2** | No mutation types — backend is read-only |
| **R3** | Clients consume only `PaneView` / events |
| **R7** | Event hub with monotonic sequence for client catch-up |

All types round-trip through `serde_json` (verified by tests in `mc-schema/src/lib.rs`) and export JSON Schema via `schemars`.