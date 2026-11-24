# Changelog

## Unreleased

- Record RUSTSEC-2026-0592 (url) and RUSTSEC-2026-0594 (cookie) as reviewed advisory exceptions until 2026-12-07, when the fixed releases have passed the seven-day rule.

## 1.1.0

- Run npm audit through `scripts/audit-npm.py` with reviewed, expiring exceptions in `npm-audit.toml`; only advisories reachable solely through devDependencies can be waived. GHSA-fc5q-39xc-vjpf (picomatch, via vite/vitest) is waived until a fixed release is available.

- `merge-when-green.py --sync` no longer switches branches when the working tree has uncommitted changes. README and SECURITY.md no longer pin the 1.0.0 version, and the advisory review date is corrected.

- Update libz-sys to 1.1.33 for RUSTSEC-2026-0544 (zlib heap buffer overflow). Record chrono being unmaintained (RUSTSEC-2026-0543) as a reviewed advisory exception until the move to jiff; the audit script now enforces the earliest exception review date.

- Update Rust dependencies, including six major versions (thiserror 2, toml 1.1, comfy-table 8, dirs 7, notify 8, crossterm 0.29), plus tokio 1.54, clap 4.6.7, Tauri dialog/fs/global-shortcut plugins and the JavaScript dialog plugin 2.8.4. The file watcher now ignores events that merely reading a repository produces (access events on Linux, directory last-access updates on Windows), so scans no longer trigger themselves.

- Add `scripts/merge-when-green.py`: waits for PR checks, squash-merges only when every check is green and Rust on Windows passed, and can dispatch and wait for an acceptance Release build.

- Speed up scans: GitHub issue/PR counts are fetched with a few batched GraphQL requests (repositories sharing a remote are looked up once) instead of one `gh` process per repository, cutting a 29-repository scan from about 4.5s to 1.2s. The GUI keeps the list visible during Refresh and drops a removed repository immediately.

- Fix scan-root discovery on Windows reporting the same worktree twice when paths differ only by 8.3 short names or separators; worktree paths are stored with native separators.

- WSL scan roots auto-follow too: the helper adds `--discover` and `--worktree-gone`, reports worktree parents and a `path_not_found` error code (new `discovers_roots`/`reports_worktrees` capabilities; older helpers are reinstalled automatically). WSL rows are shown as Not found only when the helper reports a missing path, fixing default-WSL rows misreported as Not found after other helper errors.

- Windows GUI follows scan roots automatically: repositories and linked worktrees under roots with Auto-follow are registered at startup, on Refresh and every 5 minutes; worktrees are grouped under their main repository; missing paths show as Not found with a Remove action (removal adds a scan-root exclusion and never deletes files).

- Core groundwork for scan-root auto-follow: `auto`/`worktrees` scan-root options (default on, depth default 1), linked-worktree discovery and pruning, path-based excludes, distinct "path not found" status, worktree parent reporting, and counting a worktree's unpushed branch once. `gclawl remove` now excludes the removed path from automatic discovery.

- Add `scripts/refresh-deps.py` to check dependency publication age, yanked/deprecated releases and Tauri Rust/JS alignment, and to repair notices and licence supplements; make notice generation reproducible.

- Update Tauri to 2.12 (notification plugin 2.8.0, JavaScript API/CLI 2.12.4), React 19.3, Radix Checkbox 1.4 and serde_json 1.0.154; drop advisory exceptions for crates Tauri no longer uses.

- Update rustix to 1.1.6 for RUSTSEC-2026-0486.

- Align the Windows tray icon with the application artwork while preserving status colours.

- Use Node.js 26.13.0 LTS for frontend development, CI and release builds.

- Update libz-sys to fix RUSTSEC-2026-0418 and replace the withdrawn async-executor 1.14.0 release.

- Update development and CI Node.js to 22.23.4 for CVE-2026-111497; align supported Node ranges and record runtime versions in CI (#32).

## 1.0.0

Initial public release candidate.

- Linux x86_64 CLI: list, watch, tui, repository discovery and configuration removal.
- Windows x86_64 GUI: repository status, recent commits, settings, tray and global shortcut.
- Windows-to-WSL scanning through a bundled static Linux helper.
- Uncommitted files, unpushed branches, ahead/behind and cached pull-conflict prediction.
- Optional fetch, GitHub Issue/PR counts and desktop notifications.
- MSI and portable Windows distributions; static musl Linux CLI/helper.

Limitations: unsigned Windows packages, x86_64 only, WebView2 required, WSL polling,
stale remote refs without fetch, and heuristic conflict prediction. No automatic updater
or crates.io publication. See README for configuration and compatibility boundaries.
