#!/usr/bin/env python3
"""Run npm audit for crates/gui with the reviewed exceptions in npm-audit.toml.

The audit fails when:
- `npm audit --omit=dev` reports any advisory (production dependencies are
  never waived);
- the full audit reports an advisory that has no exception, whose exception
  names a different package, or whose review date has passed;
- npm does not produce a readable report.

Exceptions that no longer match any advisory are listed so they can be removed.
"""
import datetime as dt
import json
import pathlib
import re
import subprocess
import sys
import tomllib

ROOT = pathlib.Path(__file__).resolve().parents[1]
GUI = ROOT / "crates" / "gui"
CONFIG = ROOT / "npm-audit.toml"
GHSA_URL = re.compile(r"/advisories/(GHSA-[0-9a-z]{4}-[0-9a-z]{4}-[0-9a-z]{4})$")


def advisories(report):
    """Map advisory ID -> {package, title, severity} from an `npm audit --json` report."""
    if "vulnerabilities" not in report:
        raise ValueError(f"npm audit did not return a report: {report.get('error') or report}")
    found = {}
    for entry in report["vulnerabilities"].values():
        for via in entry.get("via", []):
            if not isinstance(via, dict):
                continue  # a package name: the advisory is reported on that package
            match = GHSA_URL.search(via.get("url", ""))
            advisory_id = match[1] if match else f"npm-{via.get('source')}"
            found[advisory_id] = {
                "package": via.get("name", entry.get("name")),
                "title": via.get("title", ""),
                "severity": via.get("severity", entry.get("severity")),
            }
    return found


def evaluate(full, prod, exceptions, today):
    """Return (failures, notes) for the full and production-only reports."""
    failures, notes = [], []
    by_id = {e["id"]: e for e in exceptions}
    reported = advisories(full)
    for advisory_id, info in sorted(advisories(prod).items()):
        failures.append(f"{advisory_id} ({info['package']}, {info['severity']}) reaches "
                        f"production dependencies: {info['title']}")
    prod_ids = set(advisories(prod))
    for advisory_id, info in sorted(reported.items()):
        if advisory_id in prod_ids:
            continue
        exception = by_id.get(advisory_id)
        label = f"{advisory_id} ({info['package']}, {info['severity']})"
        if exception is None:
            failures.append(f"{label} has no reviewed exception: {info['title']}")
        elif exception["package"] != info["package"]:
            failures.append(f"{label} exception names package {exception['package']!r}")
        elif today > exception["expires"]:
            failures.append(f"{label} exception expired on {exception['expires']}; review again "
                            f"({exception['tracking']})")
        else:
            notes.append(f"waived {label} until {exception['expires']} (dev-only; "
                         f"{exception['tracking']})")
    for advisory_id in sorted(set(by_id) - set(reported)):
        notes.append(f"exception {advisory_id} no longer matches an advisory; remove it")
    return failures, notes


def npm_audit(*extra):
    proc = subprocess.run(["npm", "audit", "--json", *extra], cwd=GUI,
                          capture_output=True, text=True)
    try:
        return json.loads(proc.stdout)
    except json.JSONDecodeError as exc:
        raise SystemExit(f"npm audit {' '.join(extra)} output is not JSON ({exc}): "
                         f"{proc.stderr.strip()[:500]}") from exc


def main():
    exceptions = tomllib.loads(CONFIG.read_text()).get("exception", [])
    try:
        failures, notes = evaluate(npm_audit(), npm_audit("--omit=dev"), exceptions,
                                   dt.date.today())
    except ValueError as exc:
        raise SystemExit(str(exc)) from exc
    for note in notes:
        print(f"note: {note}")
    for failure in failures:
        print(f"FAIL: {failure}")
    if failures:
        return 1
    print("npm audit passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
