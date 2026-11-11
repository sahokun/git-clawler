# Changelog

## Unreleased

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
