# git-clawler 1.1.0

Monitor uncommitted files and unpushed branches across repositories on Linux,
Windows and WSL. Includes a Linux CLI/TUI, Windows GUI with tray/shortcuts,
recent commits, optional GitHub counts, fetch and notifications.

## Highlights

- **Scan roots follow new repositories automatically.** At startup, on Refresh and
  every 5 minutes, the Windows GUI registers repositories under each scan root,
  for native and WSL roots alike, together with their linked worktrees.
  Worktrees are grouped under their main repository with a worktree badge.
- **Missing repositories are shown, not dropped.** A repository whose folder is
  gone shows as NOT FOUND with a Remove action. Removing an automatically found
  entry adds it to the scan root's exclusions. Files on disk are never touched.
- **Faster scans.** GitHub issue/PR counts are fetched in a few batched requests.
  Refresh keeps the list on screen, and Remove takes effect immediately.
- **Platform and security updates:** Tauri 2.12, React 19.3, notify 8, a zlib fix
  (RUSTSEC-2026-0544) and other dependency updates.

## Upgrade notes

- Existing scan roots have **Auto-follow** turned on. The first launch may
  register repositories found under them, down to each root's configured depth.
  Turn off Auto-follow for a root in Settings > Scan Roots to keep managing it by hand.
- The bundled WSL helper is reinstalled automatically on first use.

## Downloads

Download the MSI for Windows installation, or extract the complete portable zip.
MSI downloads the WebView2 bootstrapper if needed; portable requires a preinstalled
WebView2 Runtime. Windows binaries are unsigned. Linux CLI and helper target x86_64 musl.
Install the Linux CLI as `gclawl` on PATH. Verify assets against SHA256SUMS.

Register with `gclawl scan ~/projects --add` or Windows Settings. Read README
for configuration, upstream semantics, remote-cache freshness and prediction limits.
LICENSE and THIRD_PARTY_NOTICES.txt accompany this release.

Windows notifications require the MSI installation; standalone portable builds do not support notifications.
