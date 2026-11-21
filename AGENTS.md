# git-clawler

<!-- Working rules for this repository, for both AI agents (Claude Code, Codex, etc.) and people.
     Rewrite the "Project-specific" section per project; keep the "Common rules" section identical across all of sahokun's repositories. -->

## Project-specific

### Overview

A cross-platform Git repository status watcher for Linux, Windows, and WSL. It monitors many repositories and surfaces uncommitted files and unpushed branches: a CLI (`gclawl`) on Linux and a native Tauri GUI on Windows, which can also watch repositories inside WSL.

`CONTRIBUTING.md` is the canonical guide for setup, checks, and the dependency policy. Keep this section short and point there instead of duplicating it.

### Tech stack and layout

- Rust workspace (toolchain pinned in `rust-toolchain.toml`) with a Tauri + npm frontend.
  - `crates/core`: Git and configuration logic. Keep it here; frontends stay thin.
  - `crates/cli`: Linux CLI.
  - `crates/gui`: Windows GUI (Tauri). Node version in `crates/gui/.nvmrc`.
  - `crates/wsl-helper`: Linux musl helper used by the GUI to reach WSL; its README defines wire compatibility.
  - `vendor/`: vendored native dependencies (libssh2 and others).
  - `scripts/`: CI, release, and dependency tooling (`refresh-deps.py`, `ci-build-scope.py`, `merge-when-green.py`, etc.).
- Use `PathBuf` for paths and run blocking Git calls on workers.
- Public documentation is canonical English; update the Japanese mirrors (`*.ja.md`) for substantial changes.
- The monthly grouped Dependabot updates are handled with the `monthly-deps` skill in `.claude/skills/`.

### Verification

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo deny check
cd crates/gui && npm ci && npm test && npm run build && cd ../..
python3 scripts/audit-npm.py
```

- Run the Rust checks on Linux and Windows for platform-specific changes, and add regression tests using temporary Git repositories for behavior changes.
- When dependencies change, run `python3 scripts/refresh-deps.py check` (seven-day rule, yanked/deprecated releases, Tauri Rust/JS alignment) and record the publication dates in the PR.

## Common rules

### Branches and PRs

<!-- branch-model:begin -->
- The default branch is `main`. Work on a branch and open a PR into `main`.
- PRs are squash-merged. Each PR leaves a single commit on main, titled with the PR title only.
<!-- branch-model:end -->
- Write PR titles in Conventional Commits form (`feat: ...`, `fix: ...`, etc.) so the change is clear from one line.
- Merged branches are deleted automatically.

### Never put these in history or anything published

- Do not write email addresses, tokens, passwords, details of a personal environment, or links to AI agent sessions in commits, PRs, issues, or comments.
  - Work-branch commits and PRs stay on GitHub after merging, and even the owner cannot delete them. Rewriting main's history needs a force push and a request to GitHub Support. Only PR descriptions can be fixed later by editing.
- Commit with your GitHub noreply address (`<ID>+<username>@users.noreply.github.com`) as the author email.
- Do not commit `.env`, key files, or local settings (`.claude/settings.local.json`, etc.).

### Dependencies

- Only add or update to versions published at least 7 days ago. Hijacked or swapped releases are often discovered shortly after publication.
  - Dependabot version updates wait 7 days via `cooldown`. For npm, `min-release-age=7` in `.npmrc` applies the same limit to manual updates.
- Pin GitHub Actions to a full 40-character commit SHA and keep the version as a trailing comment (e.g. `actions/checkout@<SHA> # v7.0.1`). Tags can be moved.
- Treat Dependabot PRs as a signal that updates exist. Merge them once CI passes, and for major updates, after reviewing the changes.
