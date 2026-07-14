# Known Gaps

Last updated: 2026-07-14

## Gap 1: Cost extraction from pi usage records

**Severity:** Medium. Cost is always reported as `$0.00`.

**Root cause:** The pi `.jsonl` records include per-message `usage` fields:
```json
{
  "usage": {
    "cost": { "total": 0.0123 },
    "input": 1234,
    "output": 567
  }
}
```
But the `RawMessage` deserialization in `collector/pi.rs` does not capture the `usage` field yet. The `PiSignals` struct accumulates `total_cost_usd` + `cost_since_last_user`, but the parsing code leaves both at 0.0.

**Fix location:** `mc-core/src/collector/pi.rs`:
1. Add `usage: Option<RawUsage>` to `RawMessage` deserialization
2. Track cumulative cost in `total_cost_usd`
3. Track delta cost since last user message in `cost_since_last_user`

**Test:** `mc status` should show non-zero costs for any pane with costs in its session log. Validate against the session with 69 tools (the stress test from the PRD).