"""Mutation tests for the release gate; no network or third-party packages."""
import pathlib
import shutil
import subprocess
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]


class ReleaseGateTests(unittest.TestCase):
    def test_unreviewed_ssh_cannot_skip_missing_provenance(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            (root / 'scripts').mkdir()
            shutil.copy(ROOT / 'scripts/check-release-security.py', root / 'scripts')
            lock = (ROOT / 'Cargo.lock').read_text()
            lock = lock.replace('0.3.3+security.20261012', '0.3.4')
            (root / 'Cargo.lock').write_text(lock)
            result = subprocess.run(['python3', str(root / 'scripts/check-release-security.py')], capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('provenance is missing', result.stderr)
            self.assertIn('libssh2', result.stderr)

    def test_ssh_version_change_still_checks_openssl_content(self):
        import hashlib
        import json
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            (root / 'scripts').mkdir()
            (root / 'vendor').mkdir()
            shutil.copy(ROOT / 'scripts/check-release-security.py', root / 'scripts')
            (root / 'Cargo.lock').write_text((ROOT / 'Cargo.lock').read_text().replace('0.3.3+security.20261012', '0.3.4'))
            (root / 'vendor/openssl.c').write_text('changed')
            provenance = {'libssh2': {'native_commit': '740c2d33ad58720db1039031d2b6e13a3df9aab8'},
                          'files': [{'path': 'openssl.c', 'sha256': hashlib.sha256(b'reviewed').hexdigest()}]}
            (root / 'vendor/PROVENANCE.json').write_text(json.dumps(provenance))
            result = subprocess.run(['python3', str(root / 'scripts/check-release-security.py')], capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('vendored source changed: openssl.c', result.stderr)

    def test_github_advisories_block_vulnerable_and_duplicate_versions(self):
        import re
        for name, vulnerable, advisory in (
            ('tauri', '2.11.0', 'GHSA-7gmj-67g7-phm9'),
            ('serde_with', '3.20.0', 'GHSA-7gcf-g7xr-8hxj'),
        ):
            for duplicate in (False, True):
                with self.subTest(name=name, duplicate=duplicate), tempfile.TemporaryDirectory() as directory:
                    root = pathlib.Path(directory)
                    (root / 'scripts').mkdir()
                    shutil.copy(ROOT / 'scripts/check-release-security.py', root / 'scripts')
                    lock = (ROOT / 'Cargo.lock').read_text()
                    if duplicate:
                        lock += f'\n[[package]]\nname = "{name}"\nversion = "{vulnerable}"\n'
                    else:
                        lock, count = re.subn(
                            rf'(name = "{name}"\nversion = ")[^"]+',
                            lambda match: match[1] + vulnerable, lock,
                        )
                        self.assertEqual(count, 1)
                    (root / 'Cargo.lock').write_text(lock)
                    result = subprocess.run(['python3', str(root / 'scripts/check-release-security.py')], capture_output=True, text=True)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn(advisory, result.stderr)

    def test_current_sources_pass(self):
        result = subprocess.run(['python3', str(ROOT / 'scripts/check-release-security.py')], capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == '__main__':
    unittest.main()
