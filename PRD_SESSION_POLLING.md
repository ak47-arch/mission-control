# PRD: Session Polling Optimization

**Status:** documented, not implemented
**Date:** 2026-07-16
**Scope:** `mc-core/src/collector/pi.rs` + `mc/src/daemon.rs`

---

## 1. Current behavior

The daemon polls every **1 second** in a tight loop (`mc/src/daemon.rs:79`):

```rust
let poll_interval = Duration::from_secs(1);
loop {
    let herdr_panes = fetch_panes(...);         // ~5ms
    let pi_signals = collect_pi_signals(...);   // reads ALL .jsonl from scratch
    let projects = collect_projects(...);       // stat() checks
    let (new_panes, new_totals) = reduce(...);  // pure function of inputs
    emit_events(...);
    sleep(poll_interval);
}
```

`collect_pi_signals` calls `parse_session(session_path)` for every pane with a valid
`agent_session_path`. That function reads the **entire** `.jsonl` file from disk,
parse every line into a `RawMessage`, build the full conversation DAG, and
recompute all derived fields — every single iteration.

```rust
fn collect_pi_signals(herdr_panes) -> HashMap<PathBuf, PiSignals> {
    for pane in herdr_panes {
        let Some(ref session_path) = pane.agent_session_path else { continue };
        if signals.contains_key(session_path) || !session_path.exists() { continue; }
        if let Ok(ps) = collector::pi::parse_session(session_path) {  // ← full re-read
            signals.insert(session_path.clone(), ps);
        }
    }
}
```

## 2. The problem

| Metric | Current value | Concern |
|--------|-------------|---------|
| Poll interval | 1s | Fine — near real-time |
| Files read per iteration | 6–10 | Depends on active panes |
| Largest session file | ~1.2 MB (~2000 turns) | Re-read 86,400×/day |
| Total daily reads | ~518,000 file opens | Wasteful I/O |
| Parse cost per large file | ~5–15ms | Adds up across panes |

**The core waste:** 99% of iterations see **zero new data** because pi hasn't written to
the session file since the last poll. Yet we open, read, and parse the entire file
every single iteration.

## 3. Why it matters

1. **CPU waste** — re-parsing a 2000-line JSONL every second burns cycles for no benefit.
   This matters more as the user accumulates sessions (more panes, longer conversations).

2. **Disk I/O** — ~500k file opens/day across all session files. On an SSD this is
   negligible today, but on HDD or a network-mounted home directory it becomes
   noticeable.

3. **GC pressure** — every `parse_session` allocates a full `ConversationTree` and
   all `RawMessage` structs, which Rust drops at the end of each iteration. With 6
   panes at 1200KB each, that's ~7 MB of allocation + deallocation per second.

4. **Scaling headroom** — as the user adds more herdr panes (15, 20, 30), the poll
   cost grows linearly with pane count × average session size.

## 4. Proposed solution

### Phase 1 — File-size caching (immediate, ~15 lines)

Track `(mtime, file_size)` per session file. Skip `parse_session` entirely if
neither has changed since last read.

```rust
struct SessionCacheEntry {
    path: PathBuf,
    last_mtime: SystemTime,
    last_size: u64,
}

fn collect_pi_signals(..., cache: &mut HashMap<PathBuf, SessionCacheEntry>) {
    for pane in herdr_panes {
        let meta = fs::metadata(session_path)?;
        if let Some(entry) = cache.get(session_path) {
            if meta.modified()? == entry.last_mtime && meta.len() == entry.last_size {
                continue;  // ← skip: no new data
            }
        }
        let ps = parse_session(session_path)?;
        cache.insert(session_path.clone(), SessionCacheEntry { ... });
        signals.insert(session_path.clone(), ps);
    }
}
```

**Impact:** Reduces parses from 86,400/day to ~86–864/day (only when pi actually writes).
A 100×–1000× reduction in work.

**Cost:** One `stat()` call per session per iteration — negligible.

### Phase 2 — Incremental tail (medium, ~50 lines)

Track the **byte offset** of the last parsed line. On each poll, seek to that offset
and only parse new lines. Append them to the existing `ConversationTree` without
re-walking the entire DAG.

```rust
struct SessionCacheEntry {
    path: PathBuf,
    last_mtime: SystemTime,
    last_size: u64,
    file_offset: u64,           // ← tracks where we left off
    cached_signals: PiSignals,   // ← mutable copy we extend
}
```

**Impact:** Turns O(n) per iteration into O(1) for idle sessions, O(k) where k is
new lines for active sessions. A 2000-line session that grows by 1 line per
iteration costs 1 parse instead of 2001.

**Risk:** If pi truncates the file (unlikely but possible for `/new`), we need to
detect it (file size < offset) and do a full reload. The orphaned-session
fallback in herdr.rs already handles the case where the session path changes.

### Phase 3 — Parse metrics + diagnostics (small, ~20 lines)

Add a `parse_duration_ms` field to the daemon's internal metrics. Expose it in
`mc diagnose` so the user can see which sessions are expensive.

```rust
let start = Instant::now();
let ps = parse_session(session_path)?;
let duration_ms = start.elapsed().as_millis();
// log or store for diagnostics
```

## 5. Non-goals

- **No file-watching (inotify/FSEvents).** The polling loop already runs; adding
  a second notification mechanism is complexity without benefit for this use case.
- **No in-memory store of full session data across daemon restarts.** The cache
  is ephemeral (built on each daemon startup). Session files are the source of truth.
- **No change to the reducer.** This is purely a collector optimization. The
  reducer sees the same `PiSignals` regardless of how quickly we produce it.

## 6. Rollout plan

| Phase | Lines | Risk | When |
|-------|-------|------|------|
| 1 — file-size cache | ~15 | None | Immediately |
| 2 — incremental tail | ~50 | Medium (truncation edge case) | After Phase 1 is stable |
| 3 — parse metrics | ~20 | None | With Phase 2 |

Phase 1 alone gives 99% of the benefit with zero risk. Phases 2–3 are polish for
users with very large sessions or very many panes.

## 7. Verification

After Phase 1:
- `mc serve` CPU usage should drop from ~5–10% to <1% when all panes are idle
- `mc diagnose` should show no regressions (same pane count, same session data)
- A new message in any pi pane should appear in the web UI within 1–2s (no
  additional delay introduced by the cache)

After Phase 2:
- A session with 3000 turns should parse in <1ms per iteration when idle
- A session growing by 1 line/sec should parse in <1ms per iteration
- Truncation (run `/new` in a pane) should trigger a full reload within 1 iteration

---

*References:*
- `mc/src/daemon.rs:72-117` — `collector_loop` and `collect_pi_signals`
- `mc-core/src/collector/pi.rs` — `parse_session` implementation
- `KNOWN_GAPS.md` — tracked gaps inventory
- `MISSION_CONTROL_PRD.md:§10` — collector design