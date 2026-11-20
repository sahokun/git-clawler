"""Check and repair dependency updates against the repository policy.

Usage:
  python3 scripts/refresh-deps.py check [--base REF] [--markdown FILE]
  python3 scripts/refresh-deps.py fix   [--base REF] [--markdown FILE]
  python3 scripts/refresh-deps.py notices

`check` compares Cargo.lock and crates/gui/package-lock.json with the base
revision (default: origin/main) and fails when an added or updated registry
version is younger than seven days, yanked or deprecated, or when Tauri Rust
crates and JavaScript packages disagree on major.minor. It changes nothing.

`fix` additionally pins too-new crates back to the newest compatible release
that satisfies the rule, regenerates THIRD_PARTY_NOTICES.txt, adds licence
supplements for crates whose texts are missing (reusing the previous version's
supplement sources at the new upstream commit), and then runs `check`.

`notices` generates THIRD_PARTY_NOTICES.txt (not committed; CI and the release
workflow generate it) and fails when a shipped package has no licence text.

Standard library only. Network access: crates.io, registry.npmjs.org,
raw.githubusercontent.com and api.github.com (unauthenticated).
"""
from __future__ import annotations

import argparse
import datetime as dt
import functools
import json
import re
import subprocess
import sys
import time
import tomllib
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CARGO_LOCK = "Cargo.lock"
NPM_LOCK = "crates/gui/package-lock.json"
MIN_AGE = dt.timedelta(days=7)
USER_AGENT = "git-clawler-dependency-check (+https://github.com/sahokun/git-clawler)"
SOURCE_RE = re.compile(r"^Source: https://github\.com/([^/]+/[^/]+)/blob/([0-9a-f]{7,40})/(\S+)$", re.M)


# --------------------------------------------------------------------------
# Lockfile parsing (pure)
# --------------------------------------------------------------------------

def cargo_packages(text: str) -> set[tuple[str, str]]:
    """Registry packages in a Cargo.lock (path/git packages are excluded)."""
    if not text:
        return set()
    return {
        (p["name"], p["version"])
        for p in tomllib.loads(text).get("package", [])
        if p.get("source", "").startswith("registry+")
    }


def npm_packages(text: str) -> dict[tuple[str, str], dict]:
    """Registry packages in an npm v3 lockfile, keyed by (name, version)."""
    if not text:
        return {}
    out = {}
    for key, entry in json.loads(text).get("packages", {}).items():
        if not key or entry.get("link") or "version" not in entry:
            continue
        name = key.rsplit("node_modules/", 1)[-1]
        out[(name, entry["version"])] = entry
    return out


def is_platform_optional(packages: dict[str, dict], key: str) -> bool:
    """Optional OS/CPU-specific packages (and their bundled children) are not
    installed on every machine, so their licence texts may be absent locally."""
    while key:
        entry = packages.get(key, {})
        if entry.get("optional") and ("os" in entry or "cpu" in entry):
            return True
        key = key.rpartition("/node_modules/")[0]
    return False


def parse_version(version: str) -> tuple[tuple[int, int, int], str]:
    core = version.split("+", 1)[0]
    core, _, pre = core.partition("-")
    parts = [int(x) for x in core.split(".")[:3]]
    while len(parts) < 3:
        parts.append(0)
    return (parts[0], parts[1], parts[2]), pre


def compatible(candidate: str, current: str) -> bool:
    """Cargo caret compatibility between two versions (same 'leftmost non-zero')."""
    (a, pre_a), (b, _) = parse_version(candidate), parse_version(current)
    if pre_a:
        return False
    if b[0] > 0:
        return a[0] == b[0]
    if b[1] > 0:
        return a[0] == 0 and a[1] == b[1]
    return a == b


def tauri_mismatches(cargo_text: str, npm_text: str) -> list[str]:
    """Tauri Rust/JS pairs whose major.minor differ (the bundler rejects them)."""
    cargo = {}
    for p in tomllib.loads(cargo_text).get("package", []):
        cargo.setdefault(p["name"], set()).add(p["version"])
    npm = {name: version for (name, version) in npm_packages(npm_text)}
    pairs = [("tauri", "@tauri-apps/api")]
    pairs += [
        (name, "@tauri-apps/plugin-" + name.removeprefix("tauri-plugin-"))
        for name in sorted(cargo)
        if name.startswith("tauri-plugin-")
    ]
    problems = []
    for rust, js in pairs:
        if rust not in cargo or js not in npm:
            continue
        js_mm = parse_version(npm[js])[0][:2]
        for version in sorted(cargo[rust]):
            if parse_version(version)[0][:2] != js_mm:
                problems.append(f"{rust} {version} (Rust) vs {js} {npm[js]} (JavaScript)")
    return problems


def normalise_supplement(parts: list[tuple[str, str, str, str]]) -> str:
    """Render a licence supplement in the repository's existing format."""
    return "\n\n".join(
        f"Source: https://github.com/{repo}/blob/{sha}/{path}\n\n{body}" for repo, sha, path, body in parts
    )


def match_renamed(wanted: str, names: list[str]) -> str | None:
    """Find a renamed licence file, e.g. LICENSE_APACHE-2.0 -> LICENSE-APACHE-2.0."""
    key = re.sub(r"[^a-z0-9]", "", wanted.lower())
    hits = [n for n in names if re.sub(r"[^a-z0-9]", "", n.lower()) == key]
    return hits[0] if len(hits) == 1 else None


# --------------------------------------------------------------------------
# Network helpers
# --------------------------------------------------------------------------

_last_crates_call = 0.0


def http_get(url: str, accept: str | None = None) -> bytes:
    headers = {"User-Agent": USER_AGENT}
    if accept:
        headers["Accept"] = accept
    with urllib.request.urlopen(urllib.request.Request(url, headers=headers), timeout=60) as response:
        return response.read()


def crates_get(path: str) -> dict:
    # crates.io asks crawlers for at most one request per second.
    global _last_crates_call
    wait = 1.0 - (time.monotonic() - _last_crates_call)
    if wait > 0:
        time.sleep(wait)
    _last_crates_call = time.monotonic()
    return json.loads(http_get("https://crates.io/api/v1/" + path))


@functools.cache
def crate_release(name: str, version: str) -> dict:
    v = crates_get(f"crates/{name}/{urllib.parse.quote(version)}")["version"]
    return {"published": v["created_at"], "yanked": v["yanked"], "license": v.get("license") or ""}


@functools.cache
def crate_versions(name: str) -> list[dict]:
    data = crates_get(f"crates/{name}/versions?per_page=100")
    return [{"num": v["num"], "published": v["created_at"], "yanked": v["yanked"]} for v in data["versions"]]


_packuments: dict[str, dict] = {}


def npm_release(name: str, version: str) -> dict:
    if name not in _packuments:
        quoted = urllib.parse.quote(name, safe="@")
        _packuments[name] = json.loads(http_get("https://registry.npmjs.org/" + quoted.replace("/", "%2F")))
    doc = _packuments[name]
    meta = doc.get("versions", {}).get(version, {})
    return {
        "published": doc.get("time", {}).get(version, ""),
        "deprecated": bool(meta.get("deprecated")),
        "license": meta.get("license") if isinstance(meta.get("license"), str) else "",
    }


# --------------------------------------------------------------------------
# Repository helpers
# --------------------------------------------------------------------------

def run(*cmd: str, check: bool = True, capture: bool = True) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, cwd=ROOT, check=check, text=True, capture_output=capture)


def git_show(ref: str, path: str) -> str:
    result = run("git", "show", f"{ref}:{path}", check=False)
    return result.stdout if result.returncode == 0 else ""


def read(path: str) -> str:
    file = ROOT / path
    return file.read_text() if file.is_file() else ""


def parse_time(value: str) -> dt.datetime:
    return dt.datetime.fromisoformat(value.replace("Z", "+00:00"))


# --------------------------------------------------------------------------
# check
# --------------------------------------------------------------------------

def evaluate(base: str, now: dt.datetime, cargo_release=crate_release, npm_release_fn=npm_release) -> dict:
    """Collect evidence for every added/updated registry version."""
    cutoff = now - MIN_AGE
    cargo_new = sorted(cargo_packages(read(CARGO_LOCK)) - cargo_packages(git_show(base, CARGO_LOCK)))
    npm_head = npm_packages(read(NPM_LOCK))
    npm_new = sorted(set(npm_head) - set(npm_packages(git_show(base, NPM_LOCK))))
    rows, problems = [], []
    for ecosystem, items, fetch in (("Rust", cargo_new, cargo_release), ("npm", npm_new, npm_release_fn)):
        for name, version in items:
            try:
                info = fetch(name, version)
            except (urllib.error.URLError, KeyError, ValueError) as error:
                problems.append(f"{ecosystem} {name} {version}: registry lookup failed ({error})")
                continue
            flags = []
            if not info["published"] or parse_time(info["published"]) > cutoff:
                flags.append("younger than 7 days")
            if info.get("yanked"):
                flags.append("yanked")
            if info.get("deprecated"):
                flags.append("deprecated")
            rows.append({"ecosystem": ecosystem, "name": name, "version": version, **info, "flags": flags})
            problems += [f"{ecosystem} {name} {version}: {flag}" for flag in flags]
    problems += ["Tauri mismatch: " + m for m in tauri_mismatches(read(CARGO_LOCK), read(NPM_LOCK))]
    return {"rows": rows, "problems": problems, "cutoff": cutoff}


def markdown(report: dict) -> str:
    lines = [f"## Publication evidence (UTC, cutoff {report['cutoff']:%Y-%m-%d %H:%M})", ""]
    for ecosystem in ("Rust", "npm"):
        rows = [r for r in report["rows"] if r["ecosystem"] == ecosystem]
        if not rows:
            continue
        lines += [
            f"<details><summary>{ecosystem} ({len(rows)})</summary>",
            "",
            "| Package | Version | Published | License | Status |",
            "| --- | --- | --- | --- | --- |",
        ]
        for r in rows:
            status = ", ".join(r["flags"]) or "ok"
            lines.append(f"| {r['name']} | {r['version']} | {r['published'][:19]}Z | {r['license']} | {status} |")
        lines += ["", "</details>", ""]
    if report["problems"]:
        lines += ["### Problems", ""] + [f"- {p}" for p in report["problems"]]
    else:
        lines.append("All added or updated versions are at least seven days old and not yanked/deprecated; Tauri Rust/JS versions align.")
    return "\n".join(lines) + "\n"


def command_check(args) -> int:
    report = evaluate(args.base, args.now)
    text = markdown(report)
    if args.markdown:
        Path(args.markdown).write_text(text)
    print(f"checked {len(report['rows'])} added/updated versions against {args.base}")
    for problem in report["problems"]:
        print("PROBLEM:", problem)
    return 1 if report["problems"] else 0


# --------------------------------------------------------------------------
# fix
# --------------------------------------------------------------------------

def pick_older(versions: list[dict], current: str, cutoff: dt.datetime) -> str | None:
    """Newest non-yanked compatible release older than `current` and the cutoff."""
    now_v = parse_version(current)[0]
    candidates = [
        v for v in versions
        if not v["yanked"]
        and parse_time(v["published"]) <= cutoff
        and compatible(v["num"], current)
        and parse_version(v["num"])[0] < now_v
    ]
    candidates.sort(key=lambda v: parse_version(v["num"])[0], reverse=True)
    return candidates[0]["num"] if candidates else None


def pin_too_new_crates(base: str, now: dt.datetime) -> list[str]:
    cutoff, notes = now - MIN_AGE, []
    for _ in range(6):
        added = sorted(cargo_packages(read(CARGO_LOCK)) - cargo_packages(git_show(base, CARGO_LOCK)))
        too_new = []
        for name, version in added:
            info = crate_release(name, version)
            if parse_time(info["published"]) > cutoff or info["yanked"]:
                too_new.append((name, version))
        if not too_new:
            return notes
        progressed = False
        for name, version in too_new:
            target = pick_older(crate_versions(name), version, cutoff)
            if not target:
                notes.append(f"no eligible older release for {name} {version}; resolve manually")
                continue
            result = run("cargo", "update", "-p", f"{name}@{version}", "--precise", target, check=False)
            if result.returncode == 0:
                notes.append(f"pinned {name} {version} -> {target}")
                progressed = True
            else:
                # The package may already have moved because of an earlier pin.
                notes.append(f"could not pin {name} {version} -> {target}: {result.stderr.strip().splitlines()[-1:]}")
        if not progressed:
            return notes
    notes.append("stopped after 6 pinning rounds; re-run fix")
    return notes


def generate_notices() -> list[str]:
    result = run(sys.executable, "scripts/generate-third-party-notices.py")
    return [line.strip() for line in result.stderr.splitlines()[1:] if line.strip()]


def registry_source_dir(name: str, version: str) -> Path | None:
    for base in sorted((Path.home() / ".cargo/registry/src").glob("*")):
        candidate = base / f"{name}-{version}"
        if candidate.is_dir():
            return candidate
    return None


def fetch_raw(repo: str, sha: str, path: str) -> str | None:
    try:
        return http_get(f"https://raw.githubusercontent.com/{repo}/{sha}/{path}").decode()
    except urllib.error.HTTPError as error:
        if error.code == 404:
            return None
        raise


def list_dir(repo: str, sha: str, directory: str) -> list[str]:
    url = f"https://api.github.com/repos/{repo}/contents/{directory}?ref={sha}"
    try:
        return [item["name"] for item in json.loads(http_get(url, "application/vnd.github+json")) if item["type"] == "file"]
    except urllib.error.HTTPError:
        return []


def add_supplement(name: str, version: str) -> str:
    target = ROOT / "licenses" / f"{name}-{version}.txt"
    if target.exists():
        return f"{target.name} already exists"
    previous = sorted(
        (ROOT / "licenses").glob(f"{name}-[0-9]*.txt"),
        key=lambda p: parse_version(p.stem[len(name) + 1:])[0],
    )
    if not previous:
        return f"{name} {version}: no earlier supplement to copy sources from; add licenses/{target.name} manually"
    sources = SOURCE_RE.findall(previous[-1].read_text())
    src = registry_source_dir(name, version)
    vcs = src / ".cargo_vcs_info.json" if src else None
    if not sources or not vcs or not vcs.is_file():
        return f"{name} {version}: missing upstream commit or sources; add licenses/{target.name} manually"
    sha = json.loads(vcs.read_text())["git"]["sha1"]
    parts = []
    for repo, _, path in sources:
        body = fetch_raw(repo, sha, path)
        if body is None:
            directory, _, file = path.rpartition("/")
            renamed = match_renamed(file, list_dir(repo, sha, directory))
            if renamed:
                path = f"{directory}/{renamed}" if directory else renamed
                body = fetch_raw(repo, sha, path)
        if body is None:
            return f"{name} {version}: {path} not found at {repo}@{sha[:10]}; add licenses/{target.name} manually"
        parts.append((repo, sha, path, body))
    target.write_text(normalise_supplement(parts))
    return f"added {target.relative_to(ROOT)} from {previous[-1].name} sources at {sha[:10]}"


def command_fix(args) -> int:
    notes = pin_too_new_crates(args.base, args.now)
    run("cargo", "fetch", "--locked", capture=False)
    missing = generate_notices()
    rust_missing = [m.removeprefix("Rust: ") for m in missing if m.startswith("Rust: ")]
    if rust_missing:
        metadata = json.loads(run("cargo", "metadata", "--locked", "--format-version", "1").stdout)
        versions = {p["name"]: p["version"] for p in metadata["packages"]}
        for name in rust_missing:
            notes.append(add_supplement(name, versions[name]))
        missing = generate_notices()
    notes += licence_gaps(missing, json.loads(read(NPM_LOCK) or "{}").get("packages", {}))
    for note in notes:
        print("-", note)
    return command_check(args)


def licence_gaps(missing: list[str], packages: dict[str, dict]) -> list[str]:
    """Licence texts that must be added before the notices are complete.

    Platform-optional npm packages are skipped: they are build tools for other
    platforms, are absent from a Linux install and are not shipped."""
    gaps = []
    for item in missing:
        if item.startswith("Rust: "):
            gaps.append(f"licence text still missing: {item}")
        elif item.startswith("npm: ") and not is_platform_optional(packages, item.removeprefix("npm: ")):
            gaps.append(f"licence text missing (check node_modules is installed): {item}")
    return gaps


def command_notices() -> int:
    """Generate THIRD_PARTY_NOTICES.txt and fail when a shipped licence text is missing."""
    run("cargo", "fetch", "--locked", capture=False)
    gaps = licence_gaps(generate_notices(), json.loads(read(NPM_LOCK) or "{}").get("packages", {}))
    for gap in gaps:
        print("-", gap)
    if gaps:
        print("PROBLEM: run `python3 scripts/refresh-deps.py fix` to add licence supplements, "
              "or add licenses/<name>-<version>.txt by hand", file=sys.stderr)
        return 1
    print("THIRD_PARTY_NOTICES.txt generated; every licence text is present")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("command", choices=("check", "fix", "notices"))
    parser.add_argument("--base", default="origin/main", help="git revision to compare with")
    parser.add_argument("--markdown", help="write the evidence table to this file")
    parser.add_argument("--now", type=lambda s: parse_time(s), default=dt.datetime.now(dt.timezone.utc), help=argparse.SUPPRESS)
    args = parser.parse_args(argv)
    if args.command == "notices":
        return command_notices()
    if run("git", "rev-parse", "--verify", "--quiet", args.base + "^{commit}", check=False).returncode:
        parser.error(f"unknown base revision {args.base!r}; fetch it first")
    return command_check(args) if args.command == "check" else command_fix(args)


if __name__ == "__main__":
    sys.exit(main())
