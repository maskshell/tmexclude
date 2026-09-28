# tmexclude — Project Instructions

## Repository Topology

- `origin` = `maskshell/tmexclude` (this fork; push target for all branches)
- `upstream` = `PhotonQuantum/tmexclude` (authoritative source of development)

## Branch Strategy

Development happens directly on the fork's `master`.

### `master` (development branch)

- Fork `master` = `upstream/master` + local development commits (environment settings, local patches, project docs including this file).
- Sync from upstream: `git fetch upstream && git rebase upstream/master`, then `git push --force-with-lease origin master`. Local commits replay on top of the new upstream tip.
- Keep commits self-contained so individual commits can be cherry-picked later.
- Never open PRs from `master` (it contains local-only commits).
- Never push to or rewrite `upstream`.

### PR branches

- Create only when a PR is wanted. Branch fresh from `upstream/master` (not from fork `master`).
- Populate by cherry-picking the needed commits from fork `master`.
- Naming: `pr/<topic>` (e.g. `pr/rule-patterns`).
- Push the PR branch to `origin`; open the PR against `upstream`.

## Local-Only Files

- Environment files (`.cgcignore`, `.env.solidforge`, similar) stay untracked via `.git/info/exclude`.
- Do not commit them and do not add them to the tracked `.gitignore`.

## Build / Test

- Rust crate root: `src-tauri/`.
- Run unit tests: `cargo test --lib` from `src-tauri/` (integration test in `tests/` is fully commented out; building the binary requires the frontend `dist/`).
- ts-rs regenerates TypeScript bindings (`src/bindings/`) during `cargo test`. Include regenerated binding files in any commit that changes exported types.
