"""Skip expensive PR builds only for an explicit list of non-build documents.

Pushes, tags, dispatches and uncertain diffs always receive the full checks.
The workflow itself still runs, so required job checks can report skipped.
"""
import os
from pathlib import Path
import subprocess

DOCUMENTS = frozenset({
    'README.md', 'README.ja.md', 'CONTRIBUTING.md', 'CONTRIBUTING.ja.md',
    'CHANGELOG.md', 'RELEASE_NOTES.md', 'SECURITY.md',
    '.github/pull_request_template.md', '.github/ISSUE_TEMPLATE/bug_report.md',
})


def requires_build(event, paths):
    return event != 'pull_request' or not paths or any(p not in DOCUMENTS for p in paths)


def main():
    event = os.environ.get('GITHUB_EVENT_NAME', '')
    paths = []
    if event == 'pull_request':
        try:
            # actions/checkout's default PR ref is GitHub's merge commit.
            # Require both parents; never use a shallow/incomplete diff to skip.
            subprocess.run(['git', 'rev-parse', '--verify', 'HEAD^2'],
                           check=True, capture_output=True)
            raw = subprocess.check_output([
                'git', 'diff', '--no-renames', '--name-only', '-z', 'HEAD^1', 'HEAD',
            ])
            paths = raw.decode('utf-8').rstrip('\0').split('\0') if raw else []
        except (subprocess.CalledProcessError, UnicodeDecodeError):
            pass
    build = str(requires_build(event, paths)).lower()
    with Path(os.environ['GITHUB_OUTPUT']).open('a') as output:
        output.write(f'build={build}\n')
    print(f'Full build required: {build}')


if __name__ == '__main__':
    main()
