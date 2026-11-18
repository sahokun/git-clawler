#!/usr/bin/env python3
"""Wait for a PR's checks, squash-merge only when they are green, and
optionally dispatch the Release workflow for an acceptance build.

Usage:
  python3 scripts/merge-when-green.py 56
  python3 scripts/merge-when-green.py 56 --release --wait-release --sync

A check counts as green when gh reports it as `pass` or `skipping`; every
`--require` check (default: Rust on Windows) must be present and `pass`.
Any `fail`/`cancel` check stops without merging. Decisions use
`gh pr checks --json` buckets, never text parsing of the table output.

Exit codes: 0 merged (and release succeeded if waited on), 1 checks or
release failed / merge refused, 2 timed out.
"""
import argparse
import datetime as dt
import json
import subprocess
import sys
import time

DEFAULT_REQUIRED = ["Rust (windows-latest)"]
GREEN = {"pass", "skipping"}
RED = {"fail", "cancel"}


def evaluate(checks, required):
    """Return ("wait" | "merge" | "fail", message) for a list of
    {"name", "bucket"} dicts."""
    if not checks:
        return "wait", "no checks reported yet"
    by_name = {c["name"]: c["bucket"] for c in checks}
    red = sorted(n for n, b in by_name.items() if b in RED)
    if red:
        return "fail", "failing checks: " + ", ".join(red)
    for name in required:
        bucket = by_name.get(name)
        if bucket in (None, "pending"):
            return "wait", f"waiting for required check {name!r}"
        if bucket != "pass":
            return "fail", f"required check {name!r} is {bucket}, not pass"
    pending = sorted(n for n, b in by_name.items() if b not in GREEN)
    if pending:
        return "wait", "pending: " + ", ".join(pending)
    return "merge", "all checks green"


def summary(checks):
    return "\n".join(f"  {c['name']}: {c['bucket']}" for c in sorted(checks, key=lambda c: c["name"]))


def gh(*args, check=True):
    proc = subprocess.run(["gh", *args], capture_output=True, text=True)
    if check and proc.returncode != 0:
        raise SystemExit(f"gh {' '.join(args)} failed: {proc.stderr.strip()}")
    return proc


def fetch_checks(pr):
    # gh exits non-zero while checks are pending or failing; stdout still has JSON.
    proc = gh("pr", "checks", str(pr), "--json", "name,bucket", check=False)
    try:
        return json.loads(proc.stdout or "[]")
    except json.JSONDecodeError:
        return []


def sync_blocker(porcelain):
    """Reason not to switch the working tree to main, or None when it is clean.

    `porcelain` is `git status --porcelain` output; switching branches with
    uncommitted changes would carry them onto main."""
    if porcelain.strip():
        return "working tree has uncommitted changes"
    return None


def find_dispatched_run(runs, since):
    """Newest workflow_dispatch run on main created at/after `since` (UTC)."""
    candidates = [
        r for r in runs
        if r.get("event") == "workflow_dispatch"
        and r.get("headBranch") == "main"
        and dt.datetime.fromisoformat(r["createdAt"].replace("Z", "+00:00")) >= since
    ]
    return max(candidates, key=lambda r: r["createdAt"], default=None)


def dispatch_release(wait):
    since = dt.datetime.now(dt.timezone.utc).replace(microsecond=0) - dt.timedelta(seconds=5)
    gh("workflow", "run", "release.yml", "--ref", "main")
    run = None
    for _ in range(30):
        time.sleep(3)
        runs = json.loads(gh("run", "list", "--workflow", "release.yml", "--limit", "5",
                             "--json", "databaseId,event,createdAt,headBranch,url").stdout)
        run = find_dispatched_run(runs, since)
        if run:
            break
    if not run:
        print("release dispatched, but the run did not appear; check Actions → Release")
        return 1
    print(f"release run: {run['url']}")
    if not wait:
        return 0
    gh("run", "watch", str(run["databaseId"]), "--exit-status", check=False)
    view = json.loads(gh("run", "view", str(run["databaseId"]), "--json", "conclusion,headSha").stdout)
    print(f"release: {view['conclusion']} (sha {view['headSha'][:7]})")
    artifacts = json.loads(gh("api", f"repos/{{owner}}/{{repo}}/actions/runs/{run['databaseId']}/artifacts").stdout)
    for a in artifacts.get("artifacts", []):
        print(f"  artifact: {a['name']} (expires {a['expires_at'][:10]})")
    return 0 if view["conclusion"] == "success" else 1


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("pr", type=int)
    p.add_argument("--require", action="append", metavar="CHECK",
                   help=f"check that must pass (repeatable; default {DEFAULT_REQUIRED})")
    p.add_argument("--no-require", action="store_true", help="no specific check is required to pass")
    p.add_argument("--timeout", type=int, default=3600, help="seconds to wait for checks")
    p.add_argument("--interval", type=int, default=20, help="seconds between polls")
    p.add_argument("--release", action="store_true", help="dispatch release.yml on main after merging")
    p.add_argument("--wait-release", action="store_true", help="with --release: wait and list artifacts")
    p.add_argument("--sync", action="store_true",
                   help="switch to and fast-forward local main after merging (skipped when the tree is dirty)")
    args = p.parse_args(argv)
    required = [] if args.no_require else (args.require or DEFAULT_REQUIRED)

    deadline = time.monotonic() + args.timeout
    last = None
    while True:
        checks = fetch_checks(args.pr)
        verdict, message = evaluate(checks, required)
        if message != last:
            print(f"[{time.strftime('%H:%M:%S')}] {message}", flush=True)
            last = message
        if verdict != "wait":
            break
        if time.monotonic() > deadline:
            print(summary(checks))
            print("timed out; not merging")
            return 2
        time.sleep(args.interval)

    print(summary(checks))
    if verdict == "fail":
        print(f"NOT MERGING. Failed logs: gh run view <run-id> --log-failed (see gh pr checks {args.pr})")
        return 1

    # gh exits non-zero when the remote merge succeeded but deleting the local
    # branch failed (e.g. a worktree still uses it), so the PR state decides.
    merge = gh("pr", "merge", str(args.pr), "--squash", "--delete-branch", check=False)
    state = json.loads(gh("pr", "view", str(args.pr), "--json", "state,mergeCommit").stdout)
    if state["state"] != "MERGED":
        print(f"merge did not complete (state {state['state']}): {merge.stderr.strip()}")
        return 1
    if merge.returncode != 0:
        print(f"merged, but gh reported: {merge.stderr.strip()}")
    print(f"MERGED {state['mergeCommit']['oid'][:7]}")

    if args.sync:
        status = subprocess.run(["git", "status", "--porcelain"], capture_output=True, text=True)
        blocker = sync_blocker(status.stdout) if status.returncode == 0 else "git status failed"
        if blocker:
            print(f"skipping local sync: {blocker}")
        else:
            for cmd in (["git", "checkout", "-q", "main"], ["git", "pull", "-q", "--ff-only"]):
                if subprocess.run(cmd).returncode != 0:
                    print(f"local sync failed at: {' '.join(cmd)}")
                    break

    if args.release:
        return dispatch_release(args.wait_release)
    return 0


if __name__ == "__main__":
    sys.exit(main())
