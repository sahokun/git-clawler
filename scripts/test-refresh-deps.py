"""Unit tests for refresh-deps.py; no network or third-party packages."""
import datetime as dt
import importlib.util
import json
import pathlib
import sys
import unittest
from unittest import mock

sys.dont_write_bytecode = True
ROOT = pathlib.Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("refresh_deps", ROOT / "scripts/refresh-deps.py")
rd = importlib.util.module_from_spec(spec)
spec.loader.exec_module(rd)

NOW = dt.datetime(2026, 11, 16, tzinfo=dt.timezone.utc)
REG = 'source = "registry+https://github.com/rust-lang/crates.io-index"'


def cargo_lock(*packages):
    body = ['version = 4']
    for name, version, *rest in packages:
        body.append(f'[[package]]\nname = "{name}"\nversion = "{version}"')
        if not rest:
            body.append(REG)
    return "\n".join(body) + "\n"


def npm_lock(**packages):
    entries = {"": {"name": "app"}}
    for key, value in packages.items():
        entries["node_modules/" + key.replace("__", "/")] = value if isinstance(value, dict) else {"version": value}
    return json.dumps({"lockfileVersion": 3, "packages": entries})


class ParsingTests(unittest.TestCase):
    def test_cargo_excludes_path_and_workspace_packages(self):
        lock = cargo_lock(("serde", "1.0.0"), ("git-clawler", "1.0.0", "local"), ("libssh2-sys", "0.3.3+security", "path"))
        self.assertEqual(rd.cargo_packages(lock), {("serde", "1.0.0")})

    def test_npm_handles_scoped_nested_and_links(self):
        lock = npm_lock(react="19.3.1", **{"@types__react": "19.3.0", "vite/node_modules/esbuild": "0.25.0",
                                            "local": {"link": True, "version": "1.0.0"}})
        self.assertEqual(set(rd.npm_packages(lock)), {("react", "19.3.1"), ("@types/react", "19.3.0"), ("esbuild", "0.25.0")})

    def test_platform_optional_detection_includes_bundled_children(self):
        packages = {
            "node_modules/oxide-wasm": {"optional": True, "cpu": ["wasm32"]},
            "node_modules/oxide-wasm/node_modules/tslib": {"optional": True, "inBundle": True},
            "node_modules/fsevents": {"optional": True, "os": ["darwin"]},
            "node_modules/maybe": {"optional": True},
            "node_modules/linux-only": {"os": ["linux"]},
        }
        for key in ("node_modules/oxide-wasm", "node_modules/oxide-wasm/node_modules/tslib", "node_modules/fsevents"):
            self.assertTrue(rd.is_platform_optional(packages, key), key)
        for key in ("node_modules/maybe", "node_modules/linux-only", "node_modules/unknown"):
            self.assertFalse(rd.is_platform_optional(packages, key), key)


class VersionTests(unittest.TestCase):
    def test_caret_compatibility(self):
        self.assertTrue(rd.compatible("0.57.1", "0.57.2"))
        self.assertFalse(rd.compatible("0.56.9", "0.57.2"))
        self.assertTrue(rd.compatible("9.1.1", "9.1.2"))
        self.assertFalse(rd.compatible("8.9.0", "9.1.2"))
        self.assertFalse(rd.compatible("9.1.1-rc.1", "9.1.2"))
        self.assertFalse(rd.compatible("0.0.3", "0.0.4"))
        self.assertTrue(rd.compatible("300.6.1+3.6.3", "300.6.2+3.6.5"))

    def test_pick_older_respects_cutoff_yank_and_compatibility(self):
        cutoff = NOW - rd.MIN_AGE
        versions = [
            {"num": "0.57.2", "published": "2026-11-10T00:00:00Z", "yanked": False},
            {"num": "0.57.1", "published": "2026-10-23T00:00:00Z", "yanked": True},
            {"num": "0.57.0", "published": "2026-10-20T00:00:00Z", "yanked": False},
            {"num": "0.56.9", "published": "2026-10-01T00:00:00Z", "yanked": False},
        ]
        self.assertEqual(rd.pick_older(versions, "0.57.2", cutoff), "0.57.0")
        self.assertIsNone(rd.pick_older(versions[:2], "0.57.2", cutoff))


class TauriAlignmentTests(unittest.TestCase):
    def test_api_and_plugin_major_minor_must_match(self):
        cargo = cargo_lock(("tauri", "2.12.5"), ("tauri-plugin-dialog", "2.8.4"), ("tauri-plugin-notification", "2.8.0"))
        npm = npm_lock(**{"@tauri-apps__api": "2.12.4", "@tauri-apps__plugin-dialog": "2.7.0"})
        problems = rd.tauri_mismatches(cargo, npm)
        self.assertEqual(len(problems), 1)
        self.assertIn("tauri-plugin-dialog 2.8.4", problems[0])

    def test_aligned_versions_pass(self):
        cargo = cargo_lock(("tauri", "2.12.5"), ("tauri-plugin-dialog", "2.7.0"))
        npm = npm_lock(**{"@tauri-apps__api": "2.12.4", "@tauri-apps__plugin-dialog": "2.7.3"})
        self.assertEqual(rd.tauri_mismatches(cargo, npm), [])


class EvaluateTests(unittest.TestCase):
    def run_evaluate(self, base_cargo, head_cargo, base_npm, head_npm, cargo_info, npm_info):
        files = {("BASE", rd.CARGO_LOCK): base_cargo, ("BASE", rd.NPM_LOCK): base_npm}
        heads = {rd.CARGO_LOCK: head_cargo, rd.NPM_LOCK: head_npm}
        with mock.patch.object(rd, "git_show", lambda ref, path: files[(ref, path)]), \
             mock.patch.object(rd, "read", lambda path: heads[path]):
            return rd.evaluate("BASE", NOW, lambda n, v: cargo_info[(n, v)], lambda n, v: npm_info[(n, v)])

    def test_flags_only_changed_versions(self):
        report = self.run_evaluate(
            cargo_lock(("serde", "1.0.0"), ("wry", "0.55.1")),
            cargo_lock(("serde", "1.0.0"), ("wry", "0.57.2"), ("brotli", "9.1.1")),
            npm_lock(react="19.2.5"), npm_lock(react="19.3.1", left="1.0.0"),
            {("wry", "0.57.2"): {"published": "2026-11-10T03:46:42Z", "yanked": False, "license": "MIT"},
             ("brotli", "9.1.1"): {"published": "2026-10-27T00:00:00Z", "yanked": True, "license": "MIT"}},
            {("react", "19.3.1"): {"published": "2026-10-14T16:47:15Z", "deprecated": False, "license": "MIT"},
             ("left", "1.0.0"): {"published": "2026-10-01T00:00:00Z", "deprecated": True, "license": "MIT"}},
        )
        self.assertEqual(len(report["rows"]), 4)
        self.assertEqual(sorted(report["problems"]), [
            "Rust brotli 9.1.1: yanked",
            "Rust wry 0.57.2: younger than 7 days",
            "npm left 1.0.0: deprecated",
        ])
        text = rd.markdown(report)
        self.assertIn("| wry | 0.57.2 | 2026-11-10T03:46:42Z | MIT | younger than 7 days |", text)
        self.assertIn("### Problems", text)

    def test_unchanged_locks_pass(self):
        lock, npm = cargo_lock(("serde", "1.0.0")), npm_lock(react="19.3.1")
        report = self.run_evaluate(lock, lock, npm, npm, {}, {})
        self.assertEqual(report["problems"], [])
        self.assertIn("All added or updated versions", rd.markdown(report))


class SupplementTests(unittest.TestCase):
    def test_format_matches_existing_supplements(self):
        rendered = rd.normalise_supplement([("o/r", "abc1234", "A", "one\n"), ("o/r", "abc1234", "B", "two\n")])
        self.assertEqual(rendered, "Source: https://github.com/o/r/blob/abc1234/A\n\none\n\n\n"
                                   "Source: https://github.com/o/r/blob/abc1234/B\n\ntwo\n")
        self.assertEqual(rd.SOURCE_RE.findall(rendered), [("o/r", "abc1234", "A"), ("o/r", "abc1234", "B")])

    def test_existing_repository_supplement_round_trips(self):
        existing = ROOT / "licenses/webview2-com-0.39.1.txt"
        sources = rd.SOURCE_RE.findall(existing.read_text())
        self.assertEqual(len(sources), 1)
        repo, sha, path = sources[0]
        body = existing.read_text().split("\n\n", 1)[1]
        self.assertEqual(rd.normalise_supplement([(repo, sha, path, body)]), existing.read_text())

    def test_renamed_licence_lookup(self):
        names = ["LICENSE-APACHE-2.0", "LICENSE-MIT", "LICENSE.spdx"]
        self.assertEqual(rd.match_renamed("LICENSE_APACHE-2.0", names), "LICENSE-APACHE-2.0")
        self.assertIsNone(rd.match_renamed("COPYING", names))


class LicenceGapTests(unittest.TestCase):
    PACKAGES = json.loads(npm_lock(
        react="19.0.0",
        **{"@rolldown__binding-win32-x64-msvc": {"version": "1.0.0", "optional": True, "os": ["win32"]}},
    ))["packages"]

    def test_platform_optional_npm_packages_are_not_gaps(self):
        self.assertEqual(rd.licence_gaps(["npm: node_modules/@rolldown/binding-win32-x64-msvc"], self.PACKAGES), [])

    def test_rust_crate_without_text_is_a_gap(self):
        self.assertEqual(rd.licence_gaps(["Rust: objc2"], self.PACKAGES), ["licence text still missing: Rust: objc2"])

    def test_shipped_npm_package_without_text_is_a_gap(self):
        gaps = rd.licence_gaps(["npm: node_modules/react"], self.PACKAGES)
        self.assertEqual(gaps, ["licence text missing (check node_modules is installed): npm: node_modules/react"])


if __name__ == "__main__":
    unittest.main()
