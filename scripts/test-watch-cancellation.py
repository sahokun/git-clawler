"""Linux PTY acceptance for initial/ongoing gh and credential-helper stalls."""
import fcntl
import http.server
import json
import os
from pathlib import Path
import pty
import signal
import struct
import subprocess
import tempfile
import termios
import threading
import time

BINARY = Path(__file__).resolve().parents[1] / 'target/debug/gclawl'


class AuthenticationRequired(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(401)
        self.send_header('WWW-Authenticate', 'Basic realm="fixture"')
        self.end_headers()

    def log_message(self, *args):
        pass


def check(action, backend, subsequent, server_port):
    with tempfile.TemporaryDirectory(prefix='gclawl-cancel-') as directory:
        root = Path(directory)
        repo = root / 'repo'
        repo.mkdir()

        def git(*args):
            subprocess.run(['git', '-C', str(repo), *args], check=True, capture_output=True)

        git('init', '-q', '-b', 'main')
        git('-c', 'user.name=Test', '-c', 'user.email=test@example.invalid',
            'commit', '--allow-empty', '-qm', 'initial')
        remote = ('https://github.com/example/example.git' if backend == 'gh'
                  else f'http://127.0.0.1:{server_port}/example.git')
        git('remote', 'add', 'origin', remote)
        binaries = root / 'bin'
        binaries.mkdir()
        helper = binaries / 'gh'
        first_call = ''
        if subsequent:
            first_call = (f'if [ ! -f "{root}/first" ]; then\n'
                          f' touch "{root}/first"\n'
                          ' printf \'%s\\n\' \'{"data":{"repository":{"issues":{"totalCount":0},'
                          '"pullRequests":{"totalCount":0}}}}\'\n exit 0\nfi\n')
        helper.write_text('#!/bin/sh\n' + first_call +
                          f'echo $$ > "{root}/helper.pid"\nsleep 90 &\n'
                          f'echo $! > "{root}/descendant.pid"\nwait\n')
        helper.chmod(0o755)
        if backend == 'fetch':
            git('config', 'credential.helper', str(helper))
            git('config', 'branch.main.remote', 'origin')
            git('config', 'branch.main.merge', 'refs/heads/main')
        config = root / 'git-clawler'
        config.mkdir()
        config.joinpath('config.toml').write_text(
            '[general]\nrefresh_interval_secs=300\n'
            f'fetch_on_refresh={str(backend == "fetch").lower()}\n'
            f'[[repositories]]\npath="{repo}"\n'
            f'[github]\nenabled={str(backend == "gh").lower()}\n')
        env = {**os.environ, 'XDG_CONFIG_HOME': str(root),
               'PATH': str(binaries) + ':' + os.environ['PATH'], 'TERM': 'xterm-256color'}
        master, slave = pty.openpty()
        initial_terminal = termios.tcgetattr(slave)
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 30, 120, 0, 0))
        process = subprocess.Popen([str(BINARY), 'watch'], env=env, stdin=slave,
                                   stdout=slave, stderr=slave, start_new_session=True)
        try:
            def wait_for(file):
                deadline = time.monotonic() + 10
                while not file.exists():
                    assert time.monotonic() < deadline, f'helper did not start: {file}'
                    time.sleep(.02)

            if subsequent:
                wait_for(root / 'first')
                time.sleep(.1)
                os.write(master, b'r')
            wait_for(root / 'descendant.pid')
            started = time.monotonic()
            if action == 'sigterm':
                process.send_signal(signal.SIGTERM)
            else:
                os.write(master, b'q' if action == 'q' else b'\x03')
            process.wait(timeout=2)
            elapsed = time.monotonic() - started
            assert process.returncode == 0
            assert termios.tcgetattr(slave) == initial_terminal, 'terminal not restored'
            time.sleep(.1)
            for name in ['helper', 'descendant']:
                pid = int(root.joinpath(name + '.pid').read_text())
                state = Path(f'/proc/{pid}/stat')
                assert not state.exists() or state.read_text().split()[2] == 'Z', f'{name} survives'
            return {'action': action, 'backend': backend, 'subsequent': subsequent,
                    'seconds': round(elapsed, 3), 'terminal_restored': True,
                    'no_running_descendants': True}
        finally:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait()
            os.close(master)
            os.close(slave)


if __name__ == '__main__':
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), AuthenticationRequired)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        results = [check(action, backend, subsequent, server.server_port)
                   for backend in ['gh', 'fetch'] for subsequent in [False, True]
                   for action in ['q', 'ctrl-c', 'sigterm']]
        print(json.dumps(results, indent=2))
    finally:
        server.shutdown()
        server.server_close()
