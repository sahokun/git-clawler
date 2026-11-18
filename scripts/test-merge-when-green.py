"""Unit tests for merge-when-green.py; no network or gh calls."""
import datetime as dt
import importlib.util
import pathlib
import sys
import unittest

sys.dont_write_bytecode = True
ROOT = pathlib.Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("mwg", ROOT / "scripts/merge-when-green.py")
mwg = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mwg)

WIN = "Rust (windows-latest)"


def c(name, bucket):
    return {"name": name, "bucket": bucket}


class Evaluate(unittest.TestCase):
    def test_all_green_merges(self):
        verdict, _ = mwg.evaluate([c(WIN, "pass"), c("Frontend", "pass"), c("dependency policy", "skipping")], [WIN])
        self.assertEqual(verdict, "merge")

    def test_failing_windows_never_merges(self):
        # The #52-#54 incident: Windows failed while everything else passed.
        verdict, message = mwg.evaluate([c(WIN, "fail"), c("Frontend", "pass")], [WIN])
        self.assertEqual(verdict, "fail")
        self.assertIn(WIN, message)

    def test_any_failure_stops_even_if_not_required(self):
        self.assertEqual(mwg.evaluate([c(WIN, "pass"), c("npm-audit", "fail")], [WIN])[0], "fail")
        self.assertEqual(mwg.evaluate([c(WIN, "pass"), c("cargo deny check", "cancel")], [WIN])[0], "fail")

    def test_pending_waits(self):
        self.assertEqual(mwg.evaluate([c(WIN, "pass"), c("Frontend", "pending")], [WIN])[0], "wait")
        self.assertEqual(mwg.evaluate([c(WIN, "pending"), c("Frontend", "pass")], [WIN])[0], "wait")

    def test_missing_required_check_waits(self):
        self.assertEqual(mwg.evaluate([c("Frontend", "pass")], [WIN])[0], "wait")

    def test_skipped_required_check_fails(self):
        verdict, message = mwg.evaluate([c(WIN, "skipping"), c("Frontend", "pass")], [WIN])
        self.assertEqual(verdict, "fail")
        self.assertIn("skipping", message)

    def test_no_required_allows_skipped_jobs(self):
        self.assertEqual(mwg.evaluate([c(WIN, "skipping"), c("Frontend", "pass")], [])[0], "merge")

    def test_no_checks_yet_waits(self):
        self.assertEqual(mwg.evaluate([], [WIN])[0], "wait")
        self.assertEqual(mwg.evaluate([], [])[0], "wait")


class SyncBlocker(unittest.TestCase):
    def test_clean_tree_allows_sync(self):
        self.assertIsNone(mwg.sync_blocker(""))
        self.assertIsNone(mwg.sync_blocker("\n"))

    def test_dirty_tree_blocks_sync(self):
        self.assertIn("uncommitted", mwg.sync_blocker(" M Cargo.toml\n?? new.txt\n"))


class FindDispatchedRun(unittest.TestCase):
    SINCE = dt.datetime(2026, 11, 17, 3, 0, tzinfo=dt.timezone.utc)

    def run_(self, rid, created, event="workflow_dispatch", branch="main"):
        return {"databaseId": rid, "createdAt": created, "event": event, "headBranch": branch, "url": f"u{rid}"}

    def test_picks_newest_dispatch_on_main_after_start(self):
        runs = [
            self.run_(1, "2026-11-17T02:59:00Z"),
            self.run_(2, "2026-11-17T03:00:10Z", event="push"),
            self.run_(3, "2026-11-17T03:00:20Z", branch="feature"),
            self.run_(4, "2026-11-17T03:00:30Z"),
        ]
        self.assertEqual(mwg.find_dispatched_run(runs, self.SINCE)["databaseId"], 4)

    def test_none_when_only_older_runs(self):
        self.assertIsNone(mwg.find_dispatched_run([self.run_(1, "2026-11-17T02:00:00Z")], self.SINCE))


if __name__ == "__main__":
    unittest.main()
