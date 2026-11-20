---
name: monthly-deps
description: Apply the monthly grouped Dependabot updates (cargo / npm / GitHub Actions) under this repository's dependency policy, using scripts/refresh-deps.py for all mechanical checks.
disable-model-invocation: true
---

# Monthly dependency update

Run when a grouped Dependabot PR (`*-monthly` for minor/patch, `*-major` for majors) arrives, or with no PR to refresh manually.
Argument: optional PR number(s). Scripts do the mechanical work; do not repeat it by hand.

## Token rules

- Trust `scripts/refresh-deps.py` output. Do not query crates.io/npm manually or re-derive dates.
- Never read `THIRD_PARTY_NOTICES.txt` or lockfile diffs in full; use `git diff --stat` and the script notes.
- Read CI logs only for failing steps (`gh run view <id> --log-failed | grep -E 'error|FAILED|panicked'`).

## Steps

1. `git switch main && git pull --ff-only && git switch -c chore/deps-$(date +%Y-%m)`.
   Use Node from `crates/gui/.nvmrc` (`PATH=$HOME/.nvm/versions/node/v$(cat crates/gui/.nvmrc)/bin:$PATH`).
2. Apply updates:
   - Dependabot cargo/npm PR: replay its changes, e.g. `gh pr diff <N> --patch | git apply`, or rerun the bumps
     (`cargo update -p <crate> --precise <ver>`, `npm install <pkg>@<ver>` in `crates/gui`).
   - GitHub Actions PR: workflow-only; review release notes for breaking changes and merge it on its own
     (needs the `workflow` scope or the web UI). Skip the remaining steps.
3. `python3 scripts/refresh-deps.py fix --markdown "${TMPDIR:-/tmp}/deps.md"` (default base `origin/main`).
   Resolve every printed note/PROBLEM:
   - npm too new → install the newest older release that is ≥ 7 days old.
   - Tauri mismatch → align the JS package major.minor with the Rust crate
     (`npm install @tauri-apps/<pkg>@^<major.minor>`), or pin the crate back.
   - "licence text missing" for a Rust crate → add `licenses/<name>-<version>.txt` by hand (format: `Source: <github blob URL>` + blank line + text).
   Re-run `fix` until it exits 0.
4. Advisory exceptions: `cargo deny check advisories 2>&1 | grep -A3 advisory-not-detected`.
   Remove an ID from `deny.toml` only if `python3 scripts/audit-rust.py` still passes without it.
5. Validate (all must pass):
   `cargo fmt --all -- --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked`,
   `cargo deny check`, `python3 scripts/audit-rust.py`, `python3 scripts/check-release-version.py`,
   `python3 scripts/check-release-security.py`, in `crates/gui`: `npm ci && npm test && npm run build`, then `python3 scripts/audit-npm.py`.
6. Add one line under `## Unreleased` in `CHANGELOG.md`. Commit (Conventional Commits, `chore(deps): ...`).
7. Push, open a PR whose body is a short change table plus the generated `deps.md`, and state that Windows GUI
   smoke testing is pending if Tauri changed. Wait for CI and squash-merge with
   `python3 scripts/merge-when-green.py <PR> --sync` (add `--release --wait-release` for a Windows acceptance
   build when Tauri changed), then close the superseded Dependabot PRs with a comment linking the new PR.

## Known pitfalls

Add an entry only when a run went wrong because this skill lacked guidance. If the same pitfall appears
twice, fix `scripts/refresh-deps.py` instead of adding text here.

- `cargo audit` does not filter by target: an advisory `cargo deny` calls "not detected" may still be needed.
- Workflow-file changes cannot be merged or pushed without the `workflow` token scope.
