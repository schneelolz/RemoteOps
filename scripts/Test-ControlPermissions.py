#!/usr/bin/env python3
"""Only ephemeral loopback Relay, credentials, and test Agent/MCP processes."""
import argparse
import os
from pathlib import Path
import secrets
import socket
import subprocess
import tempfile
import time
import uuid


def address():
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        return f'127.0.0.1:{sock.getsockname()[1]}'


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--skip-build', action='store_true')
    args = parser.parse_args()
    repo = Path(__file__).resolve().parent.parent
    env = {k: v for k, v in os.environ.items() if not k.startswith(('REMOTEOPS_', 'CONTROL_SMOKE_'))}
    if not args.skip_build:
        subprocess.run(['cargo', 'build', '--locked', '-p', 'remoteops-relay', '-p', 'remoteops-controller-mcp', '-p', 'remoteops-agent', '-p', 'remoteops-control-smoke'], cwd=repo, env=env, check=True)
    suffix = '.exe' if os.name == 'nt' else ''
    binary = lambda name: str(repo / 'target/debug' / (name + suffix))
    with tempfile.TemporaryDirectory(prefix='remoteops-control-') as directory:
        root = Path(directory)
        env.update(REMOTEOPS_HUMAN_CONTROLLER_TOKEN=secrets.token_hex(32), REMOTEOPS_AI_CONTROLLER_TOKEN=secrets.token_hex(32), REMOTEOPS_CONTROLLER_OWNER_ID=str(uuid.uuid4()),
                   CONTROL_SMOKE_ADDRESS=address(), CONTROL_SMOKE_CERT=str(root/'cert.pem'), CONTROL_SMOKE_MCP_EXECUTABLE=binary('remoteops-controller-mcp'), CONTROL_SMOKE_RELAY_STATE=str(root/'state.json'),
                   CONTROL_SMOKE_ADMIN_ADDRESS=address(), REMOTEOPS_ADMIN_TOKEN=secrets.token_hex(32))
        with (root/'relay.log').open('w') as log:
            relay = subprocess.Popen([binary('remoteops-relay'), '--bind', env['CONTROL_SMOKE_ADDRESS'], '--health-bind', address(), '--admin-bind', env['CONTROL_SMOKE_ADMIN_ADDRESS'], '--tls-cert', env['CONTROL_SMOKE_CERT'], '--tls-key', str(root/'key.pem'), '--state-file', str(root/'state.json')], cwd=root, env=env, stdout=log, stderr=log)
            try:
                for _ in range(100):
                    if relay.poll() is not None: raise RuntimeError('isolated Relay exited')
                    if (root/'cert.pem').is_file():
                        try:
                            with socket.create_connection(('127.0.0.1', int(env['CONTROL_SMOKE_ADDRESS'].split(':')[1])), timeout=.1): break
                        except OSError: pass
                    time.sleep(.1)
                else: raise RuntimeError('isolated Relay startup timed out')
                for name in ('remoteops-control-smoke', 'remoteops-control-live'):
                    subprocess.run([binary(name)], cwd=root, env=env, check=True, timeout=180)
            finally:
                relay.terminate()
                try: relay.wait(timeout=5)
                except subprocess.TimeoutExpired: relay.kill(); relay.wait()
    print('Control permission integration passed')


if __name__ == '__main__':
    main()
