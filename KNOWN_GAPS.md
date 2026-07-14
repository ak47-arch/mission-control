# Known Gaps

Last updated: 2026-07-14

## Gap 1: Cost extraction from pi usage records — **FIXED**

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
But the `RawMessage` deserialization in `collector/pi.rs` did not capture the `usage` field yet. The `PiSignals` struct accumulates `total_cost_usd` + `cost_since_last_user`, but the parsing code leaves both at 0.0.

**Fix location:** `mc-core/src/collector/pi.rs`:
1. Added `usage: Option<Usage>` to `RawMessage` deserialization
2. Track cumulative cost in `total_cost_usd`
3. Track delta cost since last user message in `cost_since_last_user`

**Status:** Fixed in commit 23df97d. `mc status` now shows non-zero costs for panes with costs in their session logs.

---

## Gap 2: Tests beyond schema round-trips

**Severity:** Low. Only serde round-trip tests exist in `mc-schema/src/lib.rs`. No reducer, collector, or transport tests yet.

**Fix location:** Add integration tests for `mc-core` (collectors, reducer, state store, transport).

---

## Gap 3: GitHub Actions CI

**Severity:** Low. Only `openwiki-update.yml` exists; no build/test workflow.

**Fix location:** Add `.github/workflows/ci.yml` with `cargo build`, `cargo test`, and `cargo clippy`.

---

## Gap 4: `rules.rs` module

**Severity:** Low. Referenced in `DESIGN.md` §1 but file does not exist; flag computation is inlined in `reducer.rs`.

**Fix location:** Extract flag logic into `mc-core/src/rules.rs`.

---

## Gap 5: Conversation arc per-turn independent walk

**Severity:** Low. TODO comment in `mc-core/src/reducer.rs:164` (`build_arc`): currently marks all historical turns as "active"; should walk each subtree independently.

**Fix location:** Refactor `build_arc` to walk each turn's subtree independently.
