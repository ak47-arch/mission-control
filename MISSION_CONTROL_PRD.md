# Mission Control — Product Requirements Document

**Status:** v0.1 — phases 0-3 implemented
**Last updated:** 2026-07-14
**Authors:** anupam + agent (chat session `019f5a2d-aac0-716d-add5-b9c4bca1f90c`)
**Scope of this doc:** architecture and contracts. Visual designs and implementation come later.

---

## 1. What we are building

A read-only birds-eye view over the user's collection of running coding-agent panes, with enough semantic context to make **manual delegation** frictionless — without doing any delegation itself.

Internally we call it **Mission Control** (`mc`). It is **not** an orchestrator. It does not send commands to agents, spawn panes, or route messages. It *collects*, *derives*, and *renders*. When (later) an orchestrator arrives, Mission Control's backend becomes a data source it can subscribe to — but the orchestrator lives outside this PRD.

Think Jarvisy: one screen that knows what every agent is doing, where each conversation is, which panes are blocked, what each project is, and which panes are waiting on you.

## 2. Goals and non-goals

### Goals
1. **See everything at a glance without switching panes.** Status, last user ask, current activity, cost, project — for every pane, in one view.
2. **Make "needs you" impossible to miss.** Blocked panes, runaway panes, panes awaiting your reply — ranked, surfaced, with the semantic reason.
3. **Be transport-pluggable.** The same backend serves a TUI, a web UI, and eventually Telegram/WhatsApp/Slack pings. No business logic in any client.
4. **Be ready for an orchestrator, not coupled to one.** The data layer is the durable asset. The orchestrator is a future client of the same events stream a TUI would be.

### Non-goals (v1)
- No agent→agent messaging, no auto-delegation, no auto-spawn. Calling `pane send-text`, `pane split`, `pane run`, or any herdr mutation from the backend is **out of scope** and out of *contract* — see §6.
- No bi-directional control. v1 is render-only. Future "act on a pane" UI affordances go through the orchestrator (§11), not through this backend.
- No re-implementing herdr. We consume herdr's API as one source among many.

## 3. The data we have (verified against the live system)

Three orthogonal sources, each with a different cadence and authority. **The architecture exists because these three do not collapse into each other.**

### 3.1 Herdr API — "what panes exist and how they look"
Unix-domain-socket JSON-RPC (mirrors the pattern in `herdr/src/api/`). Surfaces:

- `herdr pane list --workspace <id>` → per pane: `pane_id`, `tab_id`, `workspace_id`, `cwd`, `agent` (`pi`/…), `agent_status` (`idle`/`working`/`blocked`/`done`/`unknown`), `focused`, `scroll`, and crucially **`agent_session.value`** → the absolute path to this pane's pi session `.jsonl`. This path is the bridge to §3.2.
- `herdr pane read <pane_id>` → rasterized terminal bytes (visible/recent scrollback). **Authoritative for what the pane looks like**, NOT for what the agent is doing. Useful only for a "visual mirror" of the focused pane.
- `herdr wait agent-status` + `events.subscribe` → monotonic-sequence event stream for status changes. Same `events_after(seq)` pattern as `herdr/src/api/event_hub.rs`.

### 3.2 Pi session log — "what the agent is actually saying and doing"
One `.jsonl` per session at the path herdr gives us: `~/.pi/agent/sessions/<dir>/<ISO>_<UUID>.jsonl`. Verified record types:

- `session` — `{version, id, timestamp, cwd}` — session head.
- `model_change` — provider + `modelId`. Tells us "with what model".
- `thinking_level_change` — `xhigh`/`high`/… Tells us "with what intensity".
- `message` — the load-bearing record. Carries `id`, `parentId` (forms a **conversation DAG**), `timestamp`, `message.role` (`user`/`assistant`/`toolResult`), and:
  - `content[]` blocks where `type` ∈ `{text, thinking, toolCall, toolResult}`. **Note: `toolCall` (camelCase), not Anthropic's `tool_use`.** This bit us once already — the wire-reading code in §8 must key off `toolCall`/`toolResult`, `bash`/`read`/`edit`/etc. as the tool `name`.
  - `toolCall.name` + `arguments` → complete call details.
  - `toolResult.content[]` + `isError: bool` → verbatim tool output and the **blocked** signal (error with no later assistant text).
  - `usage.cost.total`, `usage.input`, `usage.output`, `reasoning`, `cacheRead`/`cacheWrite` → real per-turn accounting.

### 3.3 Per-`cwd` project scan — "what project this pane is on"
Static-ish, derived from the pane's `cwd` and rescanned only when the cwd mtime changes. Cheap, cacheable:
- Dir shape (presence of `Cargo.toml`, `package.json`, `pyproject.toml`, `docker-compose.yml`, `flake.nix`, `README.md`, `graphify-out/`, `openwiki/`, etc.) → project *kind*.
- `README.md` first non-boilerplate line / `Cargo.toml description` / `package.json description` → one-line *purpose*.
- Top-level dir list → *stack* summary.
- Presence of recently-modified dirs (e.g. `graphify-out/`, `openwiki/`) → *recent artifacts*, surfaced as "open threads" hints.

### The three-source orthogonality (this is the heart of §3)
- Herdr tells you **the pane and its state** but not what the agent is thinking.
- The pi log tells you **the agent's full semantic activity** but not which visual pane it's bound to, nor focus, nor scrollback.
- The project scan tells you **what the `cwd` means** without knowing anything about the agent.

Any two alone are insufficient. The join key is `agent_session.value` (herdr → pi log, 1:1) and `cwd` (herdr → project scan, many-panes-can-share). We persist neither copy — we keep handles and re-fetch.

## 4. The big architectural picture

```
                          ┌─────────────────────────────────────────────┐
                          │              MISSION CONTROL BACKEND          │
                          │  (long-running daemon; no UI; no mutations)  │
                          │                                              │
   herdr ─ JSON-RPC ──▶  │  ┌────────────┐   events    ┌─────────────┐   │
   (panes, status,        │  │ Collectors │──────▶────▶│  Reducer    │   │
    scrollback,           │  │ (per-source│            │ (merges →   │   │
    agent_session)        │  │  poll/tail)│            │  PaneView)  │   │
                          │  └────────────┘            └──────┬──────┘   │
   pi .jsonl ─ inotify ─▶ │     source-specific              │          │
   (messages, tools,       │     change drivers              ▼          │
    costs, errors,         │                            ┌──────────┐    │
    parent tree)           │                            │  State   │    │
                          │                            │  Store   │    │
   filesystem ─ cwd-mtime▶│                            │ (Ring +  │    │
   (project scan)          │                            │  Cursor) │    │
                          │                            └────┬─────┘    │
                          │                                 │           │
                          │  ┌─────────────────────────────┴─────────┐  │
                          │  │   Event Emitter (monotonic seq +       │  │
                          │  │   JSON-RPC over Unix socket /         │  │
                          │  │   HTTP+WS / SSE)                      │  │
                          │  └────────────┬───────────────┬──────────┘  │
                          └───────────────┼───────────────┼─────────────┘
                                          │               │
                                          ▼               ▼
              (read/query/poll)      (subscribe/stream)
                                          │               │
   ┌──────────────────┐   ┌───────────────────┐   ┌───────────────────────┐
   │   TUI client     │   │  Web UI client     │   │ Push clients         │
   │  (ratatui, runs  │   │  (browser, SSE/    │   │ (Telegram/WhatsApp/  │
   │   inside a herdr │   │  websocket, rich   │   │ Slack — notify-only, │
   │   pane itself)   │   │  graph rendering)  │   │  LLM-summarized      │
   │                  │   │                    │   │  "needs-you" only)   │
   └──────────────────┘   └─────────────────────┘   └─────────────────────-┘
                                          ▲
                                          │ (future) subscribes to same stream
                                          │
                          ┌───────────────┴────────────────┐
                          │   Orchestrator agent (future)   │
                          │   NOT in this PRD. Read-only    │
                          │   consumer of the backend today;│
                          │   mutator of herdr tomorrow.    │
                          └────────────────────────────────-┘
```

Five responsibilities, one-way arrows:

1. **Collectors** — fetch from each source using that source's natural cadence (herdr RPC poll / inotify on the jsonl dir / cwd-mtime watch). Each collector knows one source and nothing else.
2. **Reducer** — merges the three sources per pane into the canonical `PaneView` (the schema in §7). Pure function of its inputs.
3. **State store** — keeps the current `Vec<PaneView>` plus a bounded ring of historical snapshots for deltas (#7 in our brainstorm). Operated via a monotonic-sequence event log — same design as `herdr/src/api/event_hub.rs`.
4. **Event emitter** — exposes the state and events to clients over pluggable transports.
5. **Clients** — render. Never derive, never normalize, never reach into a source directly. They ask the backend.

The orchestrator of the future is **a fifth client that also writes** — see §11. We design so that role is a clean extension, not a refactor.

## 5. Separation of concerns — the load-bearing lines

These rules are what make "pluggable backend + many clients" actually work. They are non-negotiable for v1.

### R1 — Source knowledge never crosses the collector boundary.
The reducer and everything downstream must not import herdr's schema, pi's jsonl record types, or the filesystem layout. The collector emits **only** the normalized source-agnostic types in §6 ("RawSignals"). If a new pi jsonl version adds a record type, only the pi collector changes.

### R2 — The backend owns zero mutating capability.
The backend has read access to herdr RPC, the pi session dir, and the filesystem. It holds **no handle** to `pane send-text`, `pane run`, `pane split`, or any herdr mutation. This is enforced structurally, not by convention: those client bindings are not linked in. v1 is render-only by construction.

### R3 — Clients never touch sources and never touch the reducer.
A client's only allowed API surface is the Mission Control wire protocol (§8): read current `Vec<PaneView>` + subscribe to events. A web UI must not shell out to `herdr`. A TUI must not parse a pi jsonl. This is what lets clients be cheap to write and replaceable.

### R4 — Derived questions live in the reducer, not in clients.
"Is this pane blocked?" "Which panes need me?" "What's the conversation arc of pane X?" — all computed once in the reducer and exposed as `PaneView` fields. A Telegram bot that only renders "needs-you" then asks the backend for `PaneView.flags.needs_attention` and never thinks about it again.

### R5 — Transports are adapters, not business logic.
The reducer→client link is a single abstract "event stream" in code; the wire protocol (§8) is the contract. Adding Telegram means writing a transport adapter that consumes that stream and calls the Telegram API — and importing no other part of the system. Adding a new web framework must not touch the reducer.

### R6 — The data contract is versioned and is the only public surface.
`PaneView` (§7) and the event envelopes (§8) carry a `schema_version`. Clients pin a version. We can evolve the reducer freely as long as we keep serving old versions to old clients (or bump and let clients refuse). The `PaneView` schema is to Mission Control what herdr's `schema.rs` is to herdr: the keel.

### R7 — Backend process never blocks on a client.
The backend tail-pushes via subscriptions. A slow/unsubscribed client must never delay another or stall the collectors. The event hub is the buffer; clients pull `events_after(seq)` at their leisure, exactly like herdr's `EventHub`.

### R8 — Configuration is declarative and restart-safe.
What to watch (which workspaces, which cwds are interesting, cost thresholds for "runaway"), is read from a TOML/JSON config and re-read on `SIGHUP`. No live mutation of policy from a client in v1.

## 6. Source-agnostic "RawSignals" — the only thing collectors emit

Three structs, one per source. Collectors translate their source into these and send them on internal channels. **Everything past this point is source-blind.**

```rust
// From herdr collector
struct HerdrPaneSnapshot {
    workspace_id: String, workspace_label: String,
    pane_id: String, tab_id: String,
    agent: Option<String>,            // "pi", etc.
    agent_status: AgentStatus,        // Idle|Working|Blocked|Done|Unknown
    focused: bool,
    cwd: PathBuf,
    agent_session_path: Option<PathBuf>,   // bridge to PiSignals
    captured_at: Instant,
}

// From pi-session collector (one per active .jsonl, fed by inotify tail)
struct PiSignals {
    session_id: Uuid,
    session_path: PathBuf,
    started_at: DateTime<Utc>,
    cwd: PathBuf,
    // cumulative over whole session
    model: Vec<ModelId>,             // history, sorted by time; last = current
    thinking_level: Option<ThinkingLevel>,
    total_turns: u32,                // user messages count
    total_tool_calls: u32,
    total_cost_usd: f64,
    // the live, derived view
    conversation_tree: ConversationTree,  // see §7
    last_user_message: Option<String>,
    deepest_leaf_since_last_user: LeafSummary,   // current "where they are"
    tool_calls_since_last_user: u32,
    cost_since_last_user: f64,
    error_since_last_user: Option<ErrorSummary>,
    last_activity_at: DateTime<Utc>,
}

// From project-scan collector (keyed by cwd; many panes share one ProjectProfile)
struct ProjectProfile {
    cwd: PathBuf,
    kind: ProjectKind,                // Rust | Node | Python | Mixed | Unknown
    name: Option<String>,
    purpose: Option<String>,          // one-liner
    stack_summary: Vec<String>,
    recent_artifacts: Vec<ArtifactHint>,  // e.g. "graphify-out/ updated 2h ago"
    scanned_at: DateTime<Utc>,
    cwd_mtime: SystemTime,
}
```

These three are the **internal contract**. They are not the public API — clients never see them. They exist so the reducer has a stable surface regardless of which source emitted what.

## 7. The public data contract: `PaneView`

`PaneView` is the **only** thing a client needs to understand. It is the join of the three signals at a single pane. Versioned. Stable across source/transport changes. Modeled on herdr's `PaneInfo`-shaped responses.

```rust
pub const PANE_VIEW_SCHEMA_VERSION: u32 = 1;

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct PaneView {
    pub schema_version: u32,            // == PANE_VIEW_SCHEMA_VERSION
    pub pane_id: String,                // "w655ed16fb64291:p6"
    pub workspace_id: String,
    pub workspace_label: String,
    pub tab_id: String,
    pub updated_at: DateTime<Utc>,

    // ---- identity & location ----
    pub agent: Option<String>,          // "pi"
    pub agent_status: AgentStatus,
    pub focused: bool,
    pub session_id: Option<Uuid>,
    pub session_path: Option<PathBuf>,

    // ---- project (static-ish) ----
    pub project: Option<ProjectView>,   // cwd-derived; None if not scanned yet

    // ---- conversation arc (semantic) ----
    pub last_user_message: Option<String>,
    pub arc: Vec<TurnSummary>,          // last N user turns + their current state
    pub current: CurrentActivity,       // "where they are now"

    // ---- numeric vitals ----
    pub vitals: Vitals,
    pub vitals_since_last_user: VitalsDelta,

    // ---- computed flags (R4 — derived in reducer) ----
    pub flags: Flags,
}

pub struct ProjectView { /* kind, name, purpose, stack_summary, recent_artifacts */ }
pub struct TurnSummary { user: String, turns_ago: u32, ended: TurnEnd }
pub enum   TurnEnd { Answered { final_text_excerpt: String },
                     Active { current_activity: CurrentActivity, tools_so_far: u32 },
                     Errored { last_error_excerpt: String } }
pub struct CurrentActivity { kind: ActivityKind /* Thinking|ToolCall(..)|ToolResult(..)|UserPending */,
                             tool_name: Option<String>,
                             snippet: String,
                             started_at: DateTime<Utc> }
pub struct Vitals { total_turns: u32, total_tool_calls: u32, total_cost_usd: f64,
                    model: ModelId, thinking_level: Option<ThinkingLevel>,
                    session_age: Duration }
pub struct VitalsDelta { tool_calls: u32, cost_usd: f64, errors: u32 } // since last user msg

pub struct Flags {
    pub needs_attention: Attention,         // the kernel of the "needs-you" lane
    pub is_runaway: bool,                    // tools_since_last_user > RUNAWAY_THRESHOLD
    pub is_blocked: bool,                   // toolResult.isError && no assistant text after
    pub awaiting_user_reply: bool,           // agent_status Idle && has unanswered final text
    pub idle_long: Option<Duration>,         // Some when last_activity older than threshold
}
pub enum Attention { Critical /* blocked */, High /* runaway */, Medium /* awaiting reply */,
                     Low /* active working */, None /* all good */ }
```

**Design notes baked in:**
- `Flags` is computed by the reducer from `PiSignals + HerdrPaneSnapshot` per a small rules file (thresholds from config). It's the single most important client affordance — clients that only want "what needs me?" deserialize just `pane_id + flags + last_user_message` and ignore everything else.
- `current` is the **deepest leaf since last user message**, walked from `parentId` chains. (Implemented and verified in `/tmp/walk.py` against the live `019f59ea…` session — see commit history of this chat for the prototype.)
- `arc` is the last N user nodes (default 5, configurable) with their ending shape — the "where have we been" view.
- `vitals_since_last_user` answers "is this turn expensive/long?" without the client computing deltas.

## 8. The wire protocol

Two transports at v1, both consuming the **same** method/event schema. Mirrors herdr's JSON-RPC-idiolect but is independent.

### 8.1 Transport A — JSON-RPC over Unix domain socket
Why: identical access pattern to herdr itself; lets the TUI client run as a pane *inside* herdr and talk to a co-resident backend over a local socket (zero network, zero auth). Same `events_after(seq)` polling pattern as `herdr/src/api/event_hub.rs`.

Request methods (all read-only):
- `mc.snapshot` → `{ panes: Vec<PaneView>, total: Totals, sequence: u64 }`
- `mc.pane.get { pane_id }` → `PaneView`
- `mc.needs_attention` → `{ panes: Vec<PaneView> }` (subset where `Flags.attention != None`, sorted desc)
- `mc.totals` → `Totals` (cross-pane aggregates: total cost, total tools, agent counts, model histogram)
- `events.subscribe { after_seq: u64, kinds: [EventKind] }` → returns a stream of `EventEnvelope` (see below)
- `events.current_sequence` → `u64`

Events (per-pane deltas, not full snapshots, for efficiency):
```rust
pub enum EventKind {
    PaneAdded(PaneView), PaneRemoved { pane_id, sequence },
    PaneViewPatch { pane_id, sequence, patch: PaneViewPatch },  // field-level change
    AttentionChanged { pane_id, sequence, from: Attention, to: Attention },
    TotalsChanged { sequence, totals: Totals },
}
```
Patches over full snapshots are the optimization that lets 10-pane live updates stay well under 1 KB/event.

### 8.2 Transport B — HTTP + Server-Sent Events
Why: web client, and any network access at all. Same methods exposed as:
- `GET /snapshot` `GET /pane/:id` `GET /needs-attention` `GET /totals`
- `GET /events?after_seq=N&kinds=...` (SSE stream)

A thin adapter wraps the in-process event emitter into both transports. The adapter is the only place that knows HTTP exists.

### 8.3 Transport C (v1.5) — Push clients (Telegram / WhatsApp / Slack)
A bridge process that subscribes to the in-process stream, only emits on `AttentionChanged { to: Critical|High }` or on configured cadence ("daily digest"), applies an LLM-summary step to compress a `PaneView` into a chat-sized paragraph, and pushes.
**No business logic here** — it is a transform + a transport. It reads `PaneView` only.

### Wire-protocol versioning
Every response envelope carries `protocol_version`. Methods that don't exist in your version return a typed error; clients may upgrade independently of the backend.

## 9. The reducer in detail

The reducer is a pure function `(Vec<HerdrPaneSnapshot>, Map<SessionId, PiSignals>, Map<Cwd, ProjectProfile>) -> Vec<PaneView>` plus internal cache to avoid recomputing unchanged panes. It runs:

1. **Index signals** by `agent_session_path` (join key) and `cwd`.
2. **Per pane**, build `PaneView`:
   - Copy identity & status from `HerdrPaneSnapshot`.
   - If a `PiSignals` with matching `session_path` exists → merge conversation arc, vitals, deltas, and **compute `Flags`**.
   - If `cwd` maps to a `ProjectProfile` → attach `project`.
3. **Run rules** for `Flags`:
   ```
   blocked       = pi.error_since_last_user.is_some() &&
                   deepest_leaf.kind != AssistantTextLeaf
   runaway       = pi.tools_since_last_user >= config.runaway_threshold   // default 25
   awaiting_reply= herdr.agent_status == Idle &&
                   pi.deepest_leaf.kind == AssistantTextLeaf &&
                   herdr pane not focused-just-now
   idle_long     = now - pi.last_activity_at > config.idle_threshold     // default 15m
   attention     = if blocked: Critical
                   elif runaway: High
                   elif awaiting_reply: Medium
                   elif agent_status == Working: Low   // busy is fine
                   else: None
   ```
4. Emit `PaneViewPatch` events for *changed fields* since last reduce (the "delta since you last looked" affordance).
5. Recompute `Totals`.

**The reducer has no notion of clients, transports, or time-of-day. It is deterministic in its inputs.** This is what lets the same reducer feed a TUI in real time and the future orchestrator in batch.

## 10. Collectors in detail

| Collector | Source | Driver | Notes |
|---|---|---|---|
| `HerdrCollector` | herdr JSON-RPC | poll every 1s for `pane.list`; subscribe to `events` for status changes | Caches last pane list; diff to emit `PaneAdded/Removed/Patch` to internal channel |
| `PiSessionCollector` | pi `.jsonl` files | one `inotify` watcher on the session dir; tails known live files | New file → start tail; truncation/replace → reload; **never** parse all 60+ historical files, only the ones herdr names |
| `ProjectCollector` | filesystem under each `cwd` | `notify` crate on cwd mtime; rescan cheaply only on change | Per-cwd cache; many panes share one `ProjectProfile` |

All collectors speak the same language to the reducer: "here is a `HerdrPaneSnapshot` / `PiSignals` / `ProjectProfile`" with a monotonic internal seq. The reducer treats them as the three sparse inputs in §6 and never asks where they came from.

## 11. Where the orchestrator slots in (forward-compat, out of scope)

The orchestrator, when it arrives, is a **client of this backend that also writes**. We make room for it now:

- It subscribes to `PaneView` events exactly like any other client.
- It consults `Flags` to decide "what needs handling" — for free, from us.
- When it decides to act, it calls **herdr directly** (its own `herdr` client), NOT us. Our backend never gains write handles.
- Its decisions and actions can be optionally re-ingested as a fourth collector ("OrchestratorSignals": planned/intended actions, current delegation assignments) and merged into `PaneView.orchestration_state` — a future, optional field gated on a feature flag, so non-orchestrator clients see no change.

This means the v1 backend is forwards-compatible with an orchestrator through **addition**, never **mutation**: we add an OrchestratorSignals collector and an optional `PaneView.orchestration_state` field under `schema_version: 2`. No client breaks.

## 12. Why this design over alternatives considered

- **vs. "Just call herdr from the UI"** — would couple every client to herdr's wire shape and pi's wire shape, would force every client (including Telegram) to parse jsonl, and would mean doing semantic joins 3× in 3 clients. Kills the multi-client goal (R3).
- **vs. "An orchestrator with UI on top"** — conflates read and write from day one, violates the "render-only until ready" principle, and paints us into a corner if the orchestration policy turns out wrong (you'd refactor the UI you depend on). We deferral this coupling deliberately (§11).
- **vs. "A polling script that prints a table"** — fine for v0 to validate, but cannot serve a TUI, web, and push clients from one source, and cannot be observed incrementally by a future orchestrator. It's the *prototype* of this backend, not the architecture.
- **vs. "Push the logic into pi"** — would couple mission-control to one agent (pi) and one wire format (pi's jsonl v3), failing the "works for any herdr-known agent" goal. Collectors are agent-specific; everything past the collector is not.

## 13. Implementation phasing (slowly, deliberately)

Each phase is independently useful and reviewable. We do not start the next until the previous is signed off (per "implementation very slowly").

- **Phase 0 — Contract only.** Write `PaneView`, `RawSignals`, `EventKind`, `Flags` as Rust types with `serde`+`schemars`. Build the schema crate. No collectors, no transports. **Exit criterion:** the schema compiles and round-trips through `serde_json` + JSON Schema export. This is the frozen keel.
- **Phase 1 — Single-binary CLI(`mc status`).** Inline collectors + reducer, no transports, no daemon. Prints the "needs-you" lane as a plain table. Validates §6/§7/§9 against the live system. **Exit criterion:** `mc status` run from inside a herdr pane correctly emits the table we prototyped in Python in this session, live-tailed.
- **Phase 2 — Daemon + `mc` TUI client over Unix socket.** Split collectors/reducer into a long-running daemon; TUI consumes `mc.snapshot` + `events.subscribe` over Transport A. **Exit criterion:** TUI running inside a herdr pane reflects live pane state with < 200 ms update latency on status changes.
- **Phase 3 — Web client (Transport B).** Adapter, SSE, a minimalistic browser view rendering the same `PaneView` set, with the rich conversation-arc/tool-waterfall visuals that benefit from SVG. **Exit criterion:** web view at `localhost:<port>` reflects all 10 panes live.
- **Phase 4 — Push client (Telegram).** Bridge process; emit only on `AttentionChanged→Critical/High`; LLM-summarize. **Exit criterion:** a pane going blocked pings a Telegram chat with the ask + the error snippet within a few seconds.
- **Phase 5 — OrchestratorSignals collector + optional `PaneView.orchestration_state`.** Gated on a feature flag. **This is the seam where orchestration begins; before Phase 5 the system has zero write paths.**

## 14. Open questions for the planning phase (not blockers)

1. **Multi-host pi sessions.** Long term we may tail pi sessions that don't live on this machine (ssh, remote workspaces). Should `PiSessionCollector` be a trait with file/ssh/remote-api backends from day one, or YAGNI till someone needs it? *Lean:* YAGNI; add the trait in Phase 5.
2. **Cost source for non-pi agents.** For `claude`/`codex`/etc., do we get `usage.cost` from their session files? If not, `vitals.total_cost_usd` is `None` for them — fine, reducers must already treat it as optional. Investigate per-agent as we add collectors.
3. **Custom-status surfacing.** `herdr pane report-agent --custom-status` exists. Do we surface it in `PaneView` and in `Flags.attention`? *Lean:* yes, expose as a `PaneView.custom_status: Option<String>` field and let rules elevate `attention` on it.
4. **Conversation tree pruning.** A session with 200 turns means the reducer walks 200 nodes per change. Should `PiSessionCollector` keep a materialized "tail forest" (last N user roots) instead of re-walking? *Lean:* yes — but it's an internal optimization in the collector, not visible in the contract. Confirm at Phase 1 with the worst-case `survival_infra:p2` (69 tools, 43 turns — already our stress test).
5. **Auth on Transport B (HTTP).** localhost-only in v1 is fine. If we ever want LAN access (phone on the same wifi), tokens or mTLS. Defer.
6. **Where does Mission Control run?** As its own herdr pane (inception)? As a detached daemon with clients attaching? Both should work because of the socket split. Pick one default at Phase 2; the other's just `--no-attach`.

## 15. Glossary

- **PaneView** — the canonical per-pane state struct (§7), the only thing clients need to know.
- **RawSignals** — the three source-agnostic collector outputs (§6). Internal only.
- **Reducer** — pure function merging the three signals into `PaneView`s and computing `Flags` (§9).
- **Collector** — source-specific tailer/fetcher that emits raw signals (§10).
- **EventHub** — monotonic-sequence event log + `events_after(seq)` pull; mirrors herdr's design.
- **Attention** — the discrete urgency level surfaced in `PaneView.flags.attention` (Critical>High>Medium>Low>None); the foundation of the "needs-you" lane.
- **Bridge (transport adapter)** — the only component that knows about HTTP/Telegram/etc; wraps the in-process event stream (§8).
- **Orchestrator (future, out of scope)** — a future client of this backend that *also* writes to herdr; enters via Phase 5's `OrchestrationSignals` collector + an optional `PaneView` field (§11).

---

*Document history.*
- v0.1 (2026-07-14): initial draft. Verification of data sources performed live against the user's running system in session `019f5a2d-aac0-716d-add5-b9c4bca1f90c` — see `/tmp/walk.py`, `/tmp/herdr_panes.py`, and the chat transcript for evidence.
