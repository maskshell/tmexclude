---
queue_version: v1
frozen_at: 2026-09-28
plan_ref: docs/tmexclude-iteration-plan.md
authority_chain:
  - docs/tmexclude-iteration-plan.md
status: frozen
---

# Plan Queue — iteration-plan

FROZEN plan interpretation emitted by blueprint-crafting `freeze`. Read-only for the executor; revise only via the Revision Channel (`status` -> `revising` -> edit + queue_version bump -> `status: frozen`). See parallel-development `references/plan-driven-mode.md`.

## Summary (checkpoint view)

4 item(s). DoD source: docs/tmexclude-iteration-plan.md.

## Items

```json
[
  {
    "item_id": "wp-0-style",
    "seq": 0,
    "depends_on": [],
    "dod_ref": "docs/tmexclude-iteration-plan.md#dod-style",
    "title": "style: rustfmt walker.rs",
    "scope": "Standalone pure-format commit so later logic diffs stay clean (legacy let-else formatting at base).",
    "source_location": "docs/tmexclude-iteration-plan.md#items-queue-summary",
    "blueprint_subset": [],
    "producer": "blueprint-crafting",
    "plan_model_version": "v1"
  },
  {
    "item_id": "wp-1-a",
    "seq": 1,
    "depends_on": [
      "wp-0-style"
    ],
    "dod_ref": "docs/tmexclude-iteration-plan.md#dod-a",
    "title": "feat(rules): exclude-hidden pattern + protects allowlist",
    "scope": "Rule gains exclude_hidden: Option<bool> + protects: Option<Vec<PathBuf>> (serde default, ts(optional), absent=off); pinned semantics: real-dot-dirs only (symlinks not matched), if_exists gates hidden matching, protects blocks Add but not Remove; ShallowEntry carries is_dir through both walk paths; generate_diff change applies to BOTH walks; RuleItem.tsx spread-preserves at its two edit sites (type-switch site keeps fresh defaults); Hidden rule ships commented out in config.example.yaml.",
    "source_location": "docs/tmexclude-iteration-plan.md#design-decisions-key-decisions-rationale-authoritative-for-implementation",
    "blueprint_subset": [],
    "producer": "blueprint-crafting",
    "plan_model_version": "v1"
  },
  {
    "item_id": "wp-2-b",
    "seq": 2,
    "depends_on": [
      "wp-0-style"
    ],
    "dod_ref": "docs/tmexclude-iteration-plan.md#dod-b",
    "title": "perf(watcher): batch coalescing + in-flight guard + bounded pending set",
    "scope": "One spawn_blocking per delivered FSEvent batch; dedup + ancestor-first depth ordering; AtomicUsize in-flight guard (max 2, drop-guard decrement); overflow batches merge (with dedup) into a single bounded pending path set (MAX_PENDING_PATHS = 65536, cap-exceeding paths dropped with warn), the set swapping out as one batch when in-flight falls below cap; symlink_metadata deleted-path fast path; keeps existing walk_non_recursive signature.",
    "source_location": "docs/tmexclude-iteration-plan.md#design-decisions-key-decisions-rationale-authoritative-for-implementation",
    "open_decisions": [
      {
        "id": "odp-in-flight-cap",
        "kind": "deferred",
        "resolution": "MAX_IN_FLIGHT_BATCHES starts at 2; tune only if real workloads show starvation (deferred, re-surfaces downstream)."
      }
    ],
    "blueprint_subset": [],
    "producer": "blueprint-crafting",
    "plan_model_version": "v1"
  },
  {
    "item_id": "wp-3-c",
    "seq": 3,
    "depends_on": [
      "wp-1-a",
      "wp-2-b"
    ],
    "dod_ref": "docs/tmexclude-iteration-plan.md#dod-c",
    "title": "perf(walker): rule-name prefilter gated on no_include",
    "scope": "walk_non_recursive gains no_include param; after read_dir, skip per-entry getxattr when no rule-relevant name and no dot-dir is present (no_include=true); dummy Included state for if_exists markers and protects entries; watcher call site updated here.",
    "source_location": "docs/tmexclude-iteration-plan.md#design-decisions-key-decisions-rationale-authoritative-for-implementation",
    "blueprint_subset": [],
    "producer": "blueprint-crafting",
    "plan_model_version": "v1"
  }
]
```
