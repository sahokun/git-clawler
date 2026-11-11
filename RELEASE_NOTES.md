# git-clawler 1.0.0

Monitor uncommitted files and unpushed branches across repositories on Linux,
Windows and WSL. Includes a Linux CLI/TUI, Windows GUI with tray/shortcuts,
recent commits, optional GitHub counts, fetch and notifications.

Download the MSI for Windows installation, or extract the complete portable zip.
MSI downloads the WebView2 bootstrapper if needed; portable requires a preinstalled
WebView2 Runtime. Windows binaries are unsigned. Linux CLI and helper target x86_64 musl.
Install the Linux CLI as `gclawl` on PATH. Verify assets against SHA256SUMS.

Register with `gclawl scan ~/projects --add` or Windows Settings. Read README
for configuration, upstream semantics, remote-cache freshness and prediction limits.
LICENSE and THIRD_PARTY_NOTICES.txt accompany this release.

Windows notifications require the MSI installation; standalone portable builds do not support notifications.
