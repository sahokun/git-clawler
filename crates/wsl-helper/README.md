# git-clawler-wsl-helper

Linux-side one-shot scan helper for [git-clawler](../../README.md), invoked from the Windows GUI via `wsl.exe`.


## Usage

```bash
# Protocol handshake (used by GUI for version negotiation)
git-clawler-wsl-helper --protocol

# Scan repositories
git-clawler-wsl-helper /absolute/path/to/repo1 /absolute/path/to/repo2

# Fetch before scanning to improve pull-conflict prediction accuracy
git-clawler-wsl-helper --fetch /absolute/path/to/repo1 /absolute/path/to/repo2

# Discover repositories (and their linked worktrees) below a scan root
git-clawler-wsl-helper --discover /home/user/projects --depth 1 --exclude old/app [--no-worktrees]

# Report which registered worktrees Git no longer tracks (pairs in order)
git-clawler-wsl-helper --worktree-gone --worktree /tmp/app-wt --parent /home/user/projects/app
```

`--scan-root`, `--discover` and `--worktree-gone` are mutually exclusive and cannot be
combined with repository paths. A missing repository path is reported with error code
`path_not_found` (distinct from `not_a_repository`), and a linked worktree's status
includes `worktree_parent`.

## JSON Wire Format

### `--protocol` output

```json
{"protocol_version":1,"helper_version":"1.1.0"}
```

### Scan output — ok

```json
{
  "protocol_version": 1,
  "helper_version": "1.1.0",
  "scanned_at": "2026-04-27T08:05:00Z",
  "results": [
    {
      "index": 0,
      "path": "/home/user/my-repo",
      "status": {
        "kind": "ok",
        "value": {
          "current_branch": "main",
          "uncommitted_files": 2,
          "unpushed_branches": [{"name":"feat/x","ahead":3,"has_upstream":true}],
          "ahead": 3,
          "behind": 0,
          "conflict_predicted": false,
          "recent_commits": [
            {
              "short_hash": "abc1234",
              "summary": "Add recent commits UI",
              "author_name": "Alice",
              "committed_at": "2026-05-06T12:34:56Z"
            }
          ]
        }
      }
    }
  ]
}
```

### Scan output — err

```json
{
  "results": [
    {
      "index": 0,
      "path": "relative/path",
      "status": {
        "kind": "err",
        "value": {
          "code": "relative_path",
          "message": "repository path must be absolute: relative/path"
        }
      }
    }
  ]
}
```

## `--protocol` Handshake

The GUI calls `--protocol` before scanning to verify binary compatibility.
`protocol_version` is incremented only on breaking wire-format changes.
Adding new fields is non-breaking.

The optional `fetch_warning` status field contains a safe diagnostic when an optional
fetch fails. Cached status remains available. Older helpers omit this field; clients
must accept both an absent field and `null`. A successful fetch or a scan without
fetch clears the warning.

The output is a single JSON line that deserialises to the
`HelperHandshake` DTO defined in `protocol.rs`:

```json
{"protocol_version": 1, "helper_version": "1.1.0", "capabilities": {"populates_remote_url": true, "scans_roots": true, "populates_recent_commits": true, "discovers_roots": true, "reports_worktrees": true}}
```

| Field | Type | Description |
|---|---|---|
| `protocol_version` | `u32` | Wire-protocol version (used for compatibility check) |
| `helper_version` | `String` | Binary version (the `version` field in `Cargo.toml`) |
| `capabilities` | `object` | Additive capability flags (see below) |

The GUI compares only `major.minor`; a patch-level difference does not trigger a reinstall
(`crates/gui/src/wsl/bootstrap.rs::version_compatible`).
If a version mismatch is reported, compare the GUI release version, the bundled helper resource,
and the installed WSL-side `$HOME/.local/bin/git-clawler-wsl-helper --protocol` output.

### Capabilities

`capabilities` is an additive set of boolean flags. Adding a new flag is a non-breaking
wire-format change. If an older helper outputs JSON without a `capabilities` field, it
deserialises as `HelperCapabilities::default()` (all `false`).

Even when the version is compatible, the GUI bootstrap forces a helper reinstall if a
required capability flag is `false`.

| Flag | Default | Description |
|---|---|---|
| `populates_remote_url` | `false` | When `true`, each `RepoStatusDto.remote_url` is populated inside WSL via git2. The GUI uses this to run `gh api` enrichment on the Windows host without needing `gh` inside the WSL distro. Old helpers that predate this field default to `false`, triggering a forced reinstall. |
| `scans_roots` | `false` | When `true`, the helper supports `--scan-root` repository discovery. |
| `populates_recent_commits` | `false` | When `true`, each `RepoStatusDto.recent_commits` contains up to five newest `HEAD` commits. |
| `discovers_roots` | `false` | When `true`, the helper supports `--discover` (repositories plus linked worktrees) and `--worktree-gone`. |
| `reports_worktrees` | `false` | When `true`, `RepoStatusDto.worktree_parent` is populated for linked worktrees and missing paths use the `path_not_found` error code. |

## Exit Codes

| Code | Meaning |
|------|---------|
| 0 | Success (including per-path errors handled with fail-soft) |
| 1 | Fatal error (runtime build failure, JSON serialization failure) |
| 2 | CLI argument error (clap) |

## Notes

- **stdout** contains only valid JSON (1 line). `tracing` logs go to **stderr**.
- Path canonicalization is intentionally skipped (absolute paths required).
- musl cross-build (`x86_64-unknown-linux-musl`) is handled by the release workflow.
