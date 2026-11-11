"""Exercise the fail-safe PR classifier with real merge commits and deletions."""
import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).with_name('ci-build-scope.py')
spec = importlib.util.spec_from_file_location('scope', SCRIPT)
scope = importlib.util.module_from_spec(spec)
spec.loader.exec_module(scope)


class ScopeTests(unittest.TestCase):
    def test_only_known_documents_can_skip(self):
        self.assertFalse(scope.requires_build('pull_request', ['README.md', 'SECURITY.md']))
        for paths in ([], ['Cargo.lock'], ['vendor/README.md'], ['crates/core/src/lib.rs'],
                      ['.github/workflows/ci.yml'], ['README.md', 'scripts/build.py']):
            with self.subTest(paths=paths):
                self.assertTrue(scope.requires_build('pull_request', paths))
        for event in ('push', 'workflow_dispatch', 'schedule', 'workflow_call', ''):
            self.assertTrue(scope.requires_build(event, ['README.md']))

    def test_merge_diff_and_missing_parent(self):
        for delete_code in (False, True):
            with self.subTest(delete_code=delete_code), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                def git(*args):
                    return subprocess.check_output(
                        ['git', '-C', tmp, '-c', 'user.name=Test',
                         '-c', 'user.email=test@example.invalid', *args],
                        stderr=subprocess.DEVNULL,
                    )
                def classify():
                    output = root / '.git' / 'scope-output'
                    output.unlink(missing_ok=True)
                    subprocess.run(['python3', str(SCRIPT.resolve())], cwd=root,
                                   env={**os.environ, 'GITHUB_EVENT_NAME': 'pull_request',
                                        'GITHUB_OUTPUT': str(output)}, check=True,
                                   capture_output=True)
                    return output.read_text().strip()
                git('init', '-b', 'main')
                (root / 'README.md').write_text('before')
                (root / 'code.rs').write_text('source')
                git('add', '.')
                git('commit', '-m', 'base')
                self.assertEqual(classify(), 'build=true')  # No merge parents.
                git('checkout', '-b', 'pr')
                (root / 'README.md').write_text('after')
                if delete_code:
                    (root / 'code.rs').unlink()
                git('add', '-A')
                git('commit', '-m', 'change')
                git('checkout', 'main')
                git('merge', '--no-ff', '--no-edit', 'pr')
                self.assertEqual(classify(), 'build=true' if delete_code else 'build=false')


if __name__ == '__main__':
    unittest.main()
