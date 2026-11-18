"""Unit tests for audit-npm.py; no npm or network calls."""
import datetime as dt
import importlib.util
import pathlib
import sys
import tomllib
import unittest

sys.dont_write_bytecode = True
ROOT = pathlib.Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("audit_npm", ROOT / "scripts/audit-npm.py")
an = importlib.util.module_from_spec(spec)
spec.loader.exec_module(an)

TODAY = dt.date(2026, 11, 18)
GHSA = "GHSA-fc5q-39xc-vjpf"


def advisory(package, ghsa, severity="high"):
    return {"source": 1, "name": package, "title": f"{package} issue",
            "url": f"https://github.com/advisories/{ghsa}", "severity": severity}


def report(*entries):
    """entries: (package, via list)"""
    return {"auditReportVersion": 2,
            "vulnerabilities": {name: {"name": name, "severity": "high", "via": via}
                                for name, via in entries}}


EMPTY = report()
DEV_ONLY = report(("picomatch", [advisory("picomatch", GHSA)]),
                  ("vite", ["picomatch", "tinyglobby"]))


def exception(**overrides):
    base = {"id": GHSA, "package": "picomatch", "tracking": "#64",
            "expires": dt.date(2026, 12, 18), "reason": "dev only"}
    return {**base, **overrides}


class Evaluate(unittest.TestCase):
    def test_clean_reports_pass(self):
        self.assertEqual(an.evaluate(EMPTY, EMPTY, [], TODAY), ([], []))

    def test_dev_only_advisory_with_valid_exception_is_waived(self):
        failures, notes = an.evaluate(DEV_ONLY, EMPTY, [exception()], TODAY)
        self.assertEqual(failures, [])
        self.assertTrue(any("waived" in n and GHSA in n for n in notes))

    def test_advisory_without_exception_fails(self):
        failures, _ = an.evaluate(DEV_ONLY, EMPTY, [], TODAY)
        self.assertEqual(len(failures), 1)
        self.assertIn("no reviewed exception", failures[0])

    def test_production_advisory_fails_even_with_exception(self):
        failures, _ = an.evaluate(DEV_ONLY, DEV_ONLY, [exception()], TODAY)
        self.assertEqual(len(failures), 1)
        self.assertIn("production", failures[0])

    def test_expired_exception_fails(self):
        failures, _ = an.evaluate(DEV_ONLY, EMPTY, [exception(expires=dt.date(2026, 11, 17))], TODAY)
        self.assertIn("expired", failures[0])

    def test_exception_for_other_package_fails(self):
        failures, _ = an.evaluate(DEV_ONLY, EMPTY, [exception(package="minimatch")], TODAY)
        self.assertIn("names package", failures[0])

    def test_stale_exception_is_reported_as_note(self):
        failures, notes = an.evaluate(EMPTY, EMPTY, [exception()], TODAY)
        self.assertEqual(failures, [])
        self.assertTrue(any("no longer matches" in n for n in notes))

    def test_unexpected_npm_output_raises(self):
        with self.assertRaises(ValueError):
            an.evaluate({"error": {"code": "ENOTFOUND"}}, EMPTY, [], TODAY)


class Config(unittest.TestCase):
    def test_repository_exceptions_are_complete(self):
        exceptions = tomllib.loads((ROOT / "npm-audit.toml").read_text())["exception"]
        for e in exceptions:
            self.assertRegex(e["id"], r"^GHSA-")
            self.assertTrue(e["package"] and e["reason"].strip() and e["tracking"].startswith("https://"))
            self.assertIsInstance(e["expires"], dt.date)


if __name__ == "__main__":
    unittest.main()
