# tmexclude Watcher Performance & Rule Pattern — Iteration Plan

spec-version: 3

Authority: this document is the master for this change set (authority_chain[0]); on conflict with any other project doc, this document wins within its scope. Implementation semantics not covered here defer to the existing tmexclude source code.

## Problem (context — stated once, referenced elsewhere by anchor)

Diagnosis session 2026-09-28, code refs at plan-freeze base `86ce210`:

1. `uv cache prune` on `~/.cache/uv` (11 GB) under watched `~/` (user's `skips` lacked `~/.cache`) drove tmexclude CPU above 1000% and aborted the prune. tmexclude creates no files (all write paths verified: xattr/chflags via `src-tauri/src/lib/tmutil.rs`, store writes to `~/Library/Application Support`); the prune failure is a concurrent-modification race amplified by CPU starvation on uv's `remove_dir_all` (ENOTEMPTY abort, no retry).
2. Mechanism, `src-tauri/src/lib/watcher.rs`: one `spawn_blocking` per delivered event path, unbounded (tokio blocking pool grows to 512 threads).
3. Mechanism, `src-tauri/src/lib/walker.rs` `walk_non_recursive`: per-ancestor + per-entry `getxattr` over directories with thousands of entries.
4. Mechanism, `SkipCache`: only caches directories with NO applicable rule; a root-level `directories` entry (`~/`) makes every path applicable, so the cache never hits.
5. `EVENT_DELAY = 30s` is an FSEvents latency hint, not a debounce; fseventsd flushes early under sustained change volume.
6. Rule-engine gap: `Rule` matches exact entry names only (`generate_diff`: `rule.excludes.contains(name)`), so "exclude hidden (dot-prefixed) directories" cannot be expressed and per-tool cache dirs require manually maintained lists.

## Goals

- G1: tame watcher CPU under mass-deletion event storms without losing exclusion correctness.
- G2: let one rule auto-cover current and future hidden cache/state directories, with a small protected allowlist.

## Design decisions (key decisions + rationale — authoritative for implementation)

- D1 (patch A — rule patterns): `Rule` in `src-tauri/src/lib/config.rs` gains `exclude_hidden: Option<bool>` and `protects: Option<Vec<PathBuf>>` (both `#[serde(default)]` kebab-case; `#[ts(optional)]` — the pinned ts-rs fork permits `optional` only on `Option<T>`, verified 2026-09-28 against the vendored source; absent = feature off, `unwrap_or(false)` / `as_deref().unwrap_or_default()` at match sites). Pinned semantics:
  - Hidden matching applies to dot-prefixed REAL directories only. `std::fs::DirEntry::file_type` does not follow symlinks (and jwalk 0.8.1 `DirEntry` exposes a public `file_type: FileType` field, verified against the vendored crate source), so dot-prefixed symlinks are NOT hidden-matched — stated, not accidental.
  - Hidden matching is gated by the rule's `if_exists` exactly like name-based `excludes` matching.
  - The `protects` veto blocks Add actions only: an entry matching `protects` is never Add-ed, but `(expected_included, Excluded|Inconsistent)` still produces Remove, so a protected path carrying a stale exclusion xattr can be cleaned (full scans included — `generate_diff` is shared by both walks).
  - `shallow_list` values carry `is_dir` (new `ShallowEntry { state, is_dir }`), captured from `std::fs::DirEntry::file_type` in `walk_non_recursive` and from jwalk's `DirEntry.file_type` field in `walk_recursive`.
  - The `generate_diff` semantic change intentionally applies to BOTH walk paths (`walk_recursive` and `walk_non_recursive`); watcher and full scan must not disagree.
- D2 (patch B — watcher coalescing): one `spawn_blocking` per delivered FSEvent batch. Paths are deduplicated (sort + dedup) then stable-sorted by component count so ancestors process first (their apply lets descendants hit the ancestor-exclusion early return in `walk_non_recursive`). In-flight guard: `AtomicUsize` capped at `MAX_IN_FLIGHT_BATCHES = 2`, decremented by a drop guard. Overflow policy: a single bounded pending set absorbs overflowing batches while in-flight count is at the cap — the set is a deduplicated path collection (`MAX_PENDING_PATHS = 65536`); an overflowing batch's paths merge into the set (dedup), and paths that would exceed the cap are dropped with `warn!` (bounded loss, by design). When in-flight count falls below the cap and the set is non-empty, the whole set is swapped out and processed as one normal batch (same dedup + ancestor-first ordering). Deleted-path fast path: `fs::symlink_metadata(root)` error returns an empty batch before any `getxattr`.
- D3 (patch C — name prefilter): `walk_non_recursive` gains a `no_include: bool` parameter. After `read_dir` (collecting names + `is_dir` only): if no entry name is in the applicable rules' `excludes` name set AND no applicable rule has `exclude_hidden` with a dot-directory present, then under `no_include = true` return an empty batch (no Add possible, Remove forbidden). Otherwise proceed to the ancestors check and per-entry `check()`. Under `no_include = true`, an entry gets REAL `getxattr` state if and only if it is an excludes-name match OR a hidden-match candidate (dot-directory under an `exclude_hidden` rule) — this precedence resolves the marker-and-exclude overlap case; all other entries (pure `if_exists` markers, `protects` entries) enter `shallow_list` with dummy `Included` state, whose presence is all `generate_diff` reads for them. The prefilter-empty early return must NOT insert into `SkipCache` (per D4: the outcome is content-dependent, not config-deterministic).
- D4 (SkipCache semantics unchanged): caching empty-diff directories is unsound (a later event inside a cached directory would hit the cache and miss the change); only deterministic no-rule/skip directories may be cached — existing behavior is kept.
- D5 (commit stratification — range-buildability): commits land on fork master in merge order style, A, B, C and are cherry-pickable to PR branches under these declared constraints: style, A, and B are each individually buildable on `upstream/master`; C is NOT individually buildable (it consumes A's `exclude_hidden` field and updates B's watcher call site) and must be cherry-picked together with (or after) A and B. Gate-1 verifies this range-buildability property, not per-commit independence.
- D6 (frontend preservation — narrows out-of-scope): the frontend rebuilds `Rule`-shaped literals at three sites in `src/components/RuleItem.tsx` (verified 2026-09-28). Two required mitigations ship with patch A: (1) the new fields are `Option` + `#[ts(optional)]` so the existing literals stay type-valid; (2) `RuleItem.tsx` is changed to spread-preserve the previous rule object (`setValue({ ...value, excludes: ... })` pattern) at the two EDIT sites only (the excludes edit and the if-exists edit); the type-switch site (merge → concrete) keeps its fresh default object — its `value` is the Array-typed merge variant with nothing to preserve, spreading it would produce junk numeric keys, and the toggle-back path already restores the full prior object. With those two sites fixed, UI edits no longer strip hand-authored `exclude-hidden`/`protects` on save. This is the ONLY permitted frontend change.

## Complexity tiers

- style: S (pure format)
- A: M (schema + matching semantics + both walk paths + RuleItem preservation + tests)
- B: M (watcher loop restructure, guard, bounded pending set, path-prep unit)
- C: M (prefilter soundness subtleties + tests)

## Dependency edges

- style → A; style → B; A → C; B → C. (C uses A's `exclude_hidden` field and updates the watcher call site introduced by B; see D5 for the buildability constraint this imposes on cherry-picking.)

## DAG

```text
style ──▶ A ──▶ C
  └─────▶ B ──▶ C
```

Sequential merge order on fork master: style, A, B, C. No parallel fan-out (single-developer cadence; A and B are textually independent but the repo is small).

## Per-iteration DoD (definition of done per item)

- DoD-style: `rustfmt --check` clean on `walker.rs`; diff contains formatting only (no logic change).
- DoD-A: `cargo test --lib` green including new tests: hidden directory matched; dot-file NOT matched; dot-SYMLINK to a directory NOT matched; `protects` blocks Add but does NOT block Remove for a stale-excluded protected entry; `if_exists` gating applies to hidden matching; kebab-case parsing via `tests/configs/hidden_rule.yaml` (`exclude-hidden: true`, `protects` list, and absent-field → `None`). `src/bindings/Rule.ts` regenerated (fields optional) and committed. `RuleItem.tsx` spread-preserves at the two edit sites (the type-switch site keeps fresh defaults). Default behavior unchanged (`config.example.yaml` ships the Hidden rule commented out).
- DoD-B: `cargo test --lib` green including unit tests for the pure path-prep step (dedup, depth ordering) and the bounded pending-set policy (overflow merges with dedup; cap-exceeding paths dropped with warn; the set swaps out as one batch when below cap); no per-item `spawn_blocking` remains in the event loop; in-flight guard with drop-guard decrement exercised by a unit test.
- DoD-C: `cargo test --lib` green including prefilter tests (irrelevant-name directory + `no_include=true` → empty batch; `no_include=false` semantics unchanged — Remove actions still produced for stale exclusions; marker-and-exclude overlap entry still gets real state and its Add).
- DoD-integration (after all): measurable proxy for G1 — during a fixed 60-second synthetic deletion storm inside a watched temp directory (e.g. generating then deleting ≥20k files across a deep tree), the tmexclude process's average CPU stays below 200% (sampled via `ps`/`top` on the 10-core dev machine; pre-fix baseline >1000%), and the process returns to idle CPU within 30s after the storm ends. Reproduce command documented in the PR description.

## Phase acceptance gates

- Gate-1 (after style+A+B+C): full `cargo test --lib` green; `cargo clippy` clean; `rustfmt --check` clean; ts-rs bindings regenerated; range-buildability verified on a scratch branch from `upstream/master`: `git cherry-pick` style then A then B then C in order, with `cargo check` passing after style, after A, after B, and `cargo test --lib` passing after C.
- Gate-2 (user acceptance): user opts into the Hidden rule in their runtime config (`~/.config/tmexclude.yaml`, which today — plan-freeze reality — contains NO `~/.cache` skip); after a full scan and applying its result, dot-directories under `~/` (including `~/.cache`) carry the exclusion xattr; adding a new dot-directory and touching it gains the xattr automatically. Whether the user additionally keeps/adds `~/.cache` to `skips` (perf preference) is explicitly NOT gated.

## Risks and mitigations

- R1 prefilter unsoundness (missed Add action): mitigated by building the name set from the SAME applicable-rule filtering `generate_diff` uses, plus the marker-and-exclude overlap precedence in D3, plus tests; residual risk accepted (manual full scan is the corrective, and it exists and uses the same `generate_diff`).
- R2 overflow batches: the bounded pending set (D2) absorbs overflow and drains as capacity frees. Under `no_include: true` the watcher discards Remove actions anyway, so the loss mode is missed Adds, not stale Removes. Residual loss exists only where the path cap forced drops (warn-logged) or on a crash/quit before drain: those missed Adds persist until the next event on the same directory or a manual full scan (the only startup-independent corrective — no automatic startup scan exists; `Mission::new_arc` starts only `watch_task` with `kFSEventStreamEventIdSinceNow`).
- R3 frontend round-trip: CORRECTED FACT — the frontend DOES construct `Rule` literals (three sites in `RuleItem.tsx`, verified 2026-09-28); mitigations are D6 (Option + `#[ts(optional)]` keeps literals type-valid; spread-preserve stops the silent strip on save). Note the build has no `tsc` step (parcel only), so type regressions surface only in editors — D6 still fixes both the type validity and the runtime strip.
- R4 jwalk API: RESOLVED FACT — jwalk 0.8.1 `DirEntry` exposes a public `file_type: FileType` field (verified against the vendored crate source 2026-09-28); no fallback needed on that path. The std path returns `io::Result<FileType>`; `.map(|t| t.is_dir()).unwrap_or(false)` degrades conservatively (entry not hidden-matched) and, being type-info only, cannot affect excludes-name matching.
- R5 upstream drift on rebase: touched zones (`watcher.rs`, `walker.rs`, `config.rs`, `RuleItem.tsx`) are conflict-prone; mitigated by minimal, self-contained commits.

## Out of scope

- No `walk_recursive` control-flow change (plumbing for `ShallowEntry` only); the `generate_diff` semantic change intentionally applies to both walk paths (see D1).
- No default-config behavior change (Hidden rule ships commented out; users opt in).
- No frontend changes beyond the single `RuleItem.tsx` spread-preservation fix (D6) and regenerated ts-rs bindings.
- No uv-side ENOTEMPTY retry fix (upstream uv's domain).
- Watcher stays FSEvents directory-level (no `kFSEventStreamCreateFlagFileEvents`).
- No automatic startup scan (would change app lifecycle; out of this change set).

## Cross-cutting tasks

- Regenerate ts-rs bindings for every exported-type change; commit them alongside the change.
- `RuleItem.tsx` spread-preservation (D6) ships in patch A's commit.
- Every commit passes rustfmt and clippy (project fast/arch gates active).
- Tests use tempdirs only; no dependence on `~/.config` or real user state.

## Facade impact assessment

- No user-visible behavior change by default; the plan states this explicitly. Optional advisory: the commented Hidden rule block in `config.example.yaml` serves as in-repo documentation; README sync is not required for this change set.

## Items (queue summary)

- WP-0 style — rustfmt `walker.rs` (S)
- WP-1 A — rule patterns `exclude-hidden` + `protects` + RuleItem preservation (M)
- WP-2 B — watcher batch coalescing + in-flight guard + bounded pending set + deleted-path fast path (M)
- WP-3 C — `walk_non_recursive` name prefilter gated on `no_include` (M)
