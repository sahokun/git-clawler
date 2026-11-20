# Contributing to git-clawler

[日本語版](CONTRIBUTING.ja.md)

Open a pull request from your fork targeting `main`. Maintainers review successful CI
and squash merge. Use Conventional Commits and keep each change focused.

## Linux / WSL

Install Rust 1.93.1 via rustup, plus cmake, pkg-config, libssl-dev and build-essential.
The repository toolchain file selects the tested Rust version; the workspace minimum
is also 1.93.1. Build with `cargo build --workspace --locked`.
Run `cargo run --locked -p git-clawler -- list` or `scan ~/projects --add`.

## Windows GUI

Install Rust 1.93.1 (MSVC), Visual Studio C++ build tools, Windows SDK, WebView2 Runtime,
and the Node version in `crates/gui/.nvmrc`. In `crates/gui`, run `npm ci`, then
`npm run tauri -- dev`. Build the Linux musl helper first and place it at
`crates/gui/resources/wsl-helper/x86_64-unknown-linux-musl/git-clawler-wsl-helper`;
see the release workflow for cross-compilation. Run `npm run tauri -- build -- --locked`
for distribution bundles. Portable packaging uses `scripts/package-windows-portable.ps1`.

## Checks

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo deny check
cd crates/gui
npm ci
npm test
npm run build
cd ../..
python3 scripts/audit-npm.py
```

Run Rust checks on Linux and Windows for platform-specific changes. Add regression tests
for behavior changes using temporary Git repositories. Keep Git/configuration logic in
`crates/core`, frontends thin, paths as `PathBuf`, and blocking Git calls on workers.
The [helper README](crates/wsl-helper/README.md) defines wire compatibility.

## Dependencies and documentation

Verify new crates against crates.io; check maintenance, license compatibility, advisory
history and supply-chain risk. Prefer existing dependencies. Added or updated Rust/npm
versions, including transitive dependencies, must have been published for at least seven
days. Record dates and yanked/deprecated checks in the PR before validation. Audit both
Rust and npm; review vendored libgit2/OpenSSL advisories separately. Advisory exceptions
must identify a reason, affected targets, tracking issue and expiry; do not silently renew them.

`python3 scripts/refresh-deps.py check` compares the lockfiles with `origin/main` and fails
on versions younger than seven days, yanked/deprecated releases or Tauri Rust/JavaScript
major.minor mismatches; `--markdown FILE` writes the publication table for the PR.
`python3 scripts/refresh-deps.py fix` also pins too-new crates to the newest eligible
release and adds licence supplements for new crate versions from the previous
supplement's sources. Review its notes; npm versions and Tauri mismatches are reported,
not changed.

`THIRD_PARTY_NOTICES.txt` is not committed. The release workflow generates it from the
lockfiles with `python3 scripts/refresh-deps.py notices` and ships it with every release
asset. The dependency policy check runs the same command on pull requests and fails when
a shipped package has no licence text; add the supplement with `fix` (or by hand in
`licenses/`). Generate it locally (after `npm ci` in `crates/gui`) before running
`scripts/package-windows-portable.ps1`.

Public documentation is canonical English; update the Japanese mirrors for substantial
changes. Document CLI/TOML compatibility changes. Never include credentials, personal
configuration, build output or generated helper binaries in a PR.

## Node.js security baseline

Use the latest validated LTS, Node.js 26.13.0 from `crates/gui/.nvmrc` for development and CI. The declared
Node 22/24/26 ranges start at the CVE-2026-111497 patched releases; this change
was validated on Node 26.13.0. With nvm, run `nvm install && nvm use` in
`crates/gui`; on Windows use `nvm install 26.13.0` and `nvm use 26.13.0`.
Check `node --version` and `node -p "process.execPath"` in a new terminal and
restart development processes after switching. GitHub Actions use a separate
runner-managed Node runtime, so keep the runner updated as well.
