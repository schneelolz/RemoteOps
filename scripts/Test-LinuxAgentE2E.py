#!/usr/bin/env python3
"""Isolated, non-root Linux MCP/Relay/Agent integration acceptance.

Never uses production credentials or restarts a system service. JSON output contains
test names/results only; private fixture state stays in a mode-0700 temp directory.
Build relay + controller-mcp (debug), agent + agent-service (release) first.
"""
import argparse
import base64
import hashlib
import json
import os
import pty
from pathlib import Path
import queue
import secrets
import signal
import socket
import subprocess
import tempfile
import threading
import time
import uuid


def wait_for(predicate, timeout=40):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        try:
            value = predicate()
            if value:
                return value
        except (OSError, ValueError, KeyError):
            pass
        time.sleep(0.25)
    raise AssertionError('condition timed out')


def port():
    with socket.socket() as s:
        s.bind(('127.0.0.1', 0))
        return s.getsockname()[1]


class Mcp:
    def __init__(self, args, env, log):
        self.proc = subprocess.Popen(args, env=env, stdin=subprocess.PIPE,
                                     stdout=subprocess.PIPE, stderr=log, text=True)
        self.messages = {}
        self.lock = threading.Lock()
        self.serial = 0
        self.elicitations = 0
        threading.Thread(target=self.read, daemon=True).start()
        self.request('initialize', {'protocolVersion': '2025-03-26',
                     'capabilities': {'elicitation': {'form': {}}},
                     'clientInfo': {'name': 'linux-acceptance', 'version': '1'}})
        self.send({'jsonrpc': '2.0', 'method': 'notifications/initialized'})

    def read(self):
        for line in self.proc.stdout:
            msg = json.loads(line)
            if 'method' in msg and 'id' in msg:
                if msg['method'] == 'elicitation/create':
                    self.elicitations += 1
                    result = {'action': 'decline'}
                else:
                    result = {}
                self.send({'jsonrpc': '2.0', 'id': msg['id'], 'result': result})
            elif msg.get('id') in self.messages:
                self.messages[msg['id']].put(msg)

    def send(self, message):
        with self.lock:
            self.proc.stdin.write(json.dumps(message) + '\n')
            self.proc.stdin.flush()

    def request(self, method, params):
        with self.lock:
            self.serial += 1
            ident = self.serial
            inbox = self.messages[ident] = queue.Queue()
        self.send({'jsonrpc': '2.0', 'id': ident, 'method': method, 'params': params})
        msg = inbox.get(timeout=90)
        self.messages.pop(ident)
        if 'error' in msg:
            return {'isError': True, 'error': msg['error']}
        return msg['result']

    def tool(self, name, **args):
        result = self.request('tools/call', {'name': name, 'arguments': args})
        if result.get('isError'):
            return result
        if 'structuredContent' in result:
            return result['structuredContent']
        for item in result.get('content', []):
            if item.get('type') == 'text':
                try:
                    return json.loads(item['text'])
                except ValueError:
                    pass
        return result


class RelayLink:
    """Loopback transport fixture: drop sockets without changing host networking."""
    def __init__(self, target):
        self.target = target
        self.sockets = []
        self.listener = socket.socket()
        self.listener.bind(('127.0.0.1', 0))
        self.listener.listen()
        self.port = self.listener.getsockname()[1]
        threading.Thread(target=self.accept, daemon=True).start()

    def accept(self):
        while True:
            try:
                incoming, _ = self.listener.accept()
            except OSError:
                return
            try:
                outgoing = socket.create_connection(('127.0.0.1', self.target))
            except OSError:
                incoming.close()
                continue
            self.sockets.extend([incoming, outgoing])
            for a, b in [(incoming, outgoing), (outgoing, incoming)]:
                threading.Thread(target=self.forward, args=(a, b), daemon=True).start()

    @staticmethod
    def forward(a, b):
        try:
            while chunk := a.recv(65536):
                b.sendall(chunk)
        except OSError:
            pass
        finally:
            for s in (a, b):
                try:
                    s.shutdown(socket.SHUT_RDWR)
                except OSError:
                    pass
                s.close()

    def drop(self):
        for s in self.sockets:
            try:
                s.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
            s.close()
        self.sockets.clear()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--root', type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    if os.geteuid() == 0 or os.uname().sysname != 'Linux':
        raise SystemExit('Run as a non-root Linux user.')
    root = args.root.resolve()
    results = []
    processes = []
    logs = []
    os.umask(0o077)
    fixture = Path(tempfile.mkdtemp(prefix='remoteops-linux-e2e-'))
    env = {k: v for k, v in os.environ.items() if not k.startswith('REMOTEOPS_')}
    token = secrets.token_hex(32)
    env.update(REMOTEOPS_AI_CONTROLLER_TOKEN=token,
               REMOTEOPS_HUMAN_CONTROLLER_TOKEN=secrets.token_hex(32),
               REMOTEOPS_CONTROLLER_TOKEN=token,
               REMOTEOPS_CONTROLLER_OWNER_ID=str(uuid.uuid4()))
    relay_port, health_port = port(), port()
    relay_args = [str(root/'target/debug/remoteops-relay'), '--bind', f'127.0.0.1:{relay_port}',
                  '--health-bind', f'127.0.0.1:{health_port}', '--tls-cert', str(fixture/'cert.pem'),
                  '--tls-key', str(fixture/'key.pem'), '--state-file', str(fixture/'relay.json'),
                  '--heartbeat-seconds', '1', '--lease-seconds', '30']
    status = fixture/'status.json'
    config = {'relay': f'127.0.0.1:{relay_port}', 'server_name': 'localhost',
              'ca_cert': str(fixture/'cert.pem'), 'transfer_root': str(fixture/'agent-files'),
              'state_file': str(fixture/'agent.json'), 'retry_seconds': 1}
    (fixture/'config.json').write_text(json.dumps(config))
    (fixture/'mcp-files').mkdir()
    agent_args = [str(root/'target/release/remoteops-agent-service'), '--console',
                  '--config', str(fixture/'config.json'), '--status-file', str(status)]

    def spawn(command, name):
        log = (fixture/(name+'.log')).open('a')
        logs.append(log)
        p = subprocess.Popen(command, env=env, stdout=log, stderr=log)
        processes.append(p)
        return p

    def stop(p, sig=signal.SIGTERM):
        if p.poll() is None:
            p.send_signal(sig)
            p.wait(timeout=15)

    def check(name, predicate):
        assert predicate, name
        results.append({'test': name, 'status': 'passed'})
        print('PASS '+name, flush=True)

    def paired_status():
        data = json.loads(status.read_text())
        return data if data.get('pairing_code') else None

    try:
        relay = spawn(relay_args, 'relay')
        wait_for(lambda: (fixture/'cert.pem').is_file())
        link = RelayLink(relay_port)
        config['relay'] = f'127.0.0.1:{link.port}'
        (fixture/'config.json').write_text(json.dumps(config))
        agent = spawn(agent_args, 'agent')
        initial = wait_for(paired_status)
        check('private runtime status', status.stat().st_mode & 0o777 == 0o600)
        log = (fixture/'mcp.log').open('a'); logs.append(log)
        mcp = Mcp([str(root/'target/debug/remoteops-controller-mcp'), '--relay', f'127.0.0.1:{relay_port}',
                   '--server-name', 'localhost', '--ca-cert', str(fixture/'cert.pem'),
                   '--audit-log', str(fixture/'audit.jsonl'), '--transfer-root', str(fixture/'mcp-files'),
                   '--reconnect-seconds', '1'], env, log)
        processes.append(mcp.proc)
        paired = mcp.tool('pair_connection', pairing_code=initial['pairing_code'])
        sid = paired['session_id']

        def tool(name, **kw):
            return mcp.tool(name, session_id=sid, **kw)

        def command(text, **kw):
            return tool('run_command', command=text, **({'shell': 'system'} if not kw else kw))

        def succeeded(value):
            return not value.get('isError') and value.get('status') == 'completed' and value.get('exit_code') in (0, None)

        def output():
            return json.dumps(tool('read_output', limit=500), ensure_ascii=False)

        check('pair and Linux environment', tool('get_target_info')['environment']['os_family'] == 'linux')
        denied = command('touch '+str(fixture/'must-not-exist'))
        check('step-by-step approval decline', denied.get('isError') and not (fixture/'must-not-exist').exists())
        check('approval actually requested', mcp.elicitations > 0)
        check('explicit full access', tool('set_control_mode', mode='full_access')['mode'] == 'full_access')
        check('POSIX shell UTF-8 ANSI', succeeded(command("printf '\033[31m中文验收\033[0m\\n'")))
        check('UTF-8 output available', '中文验收' in output())
        chunks = [e['payload'].get('text', '') for e in tool('read_output', limit=500)['events'] if e['payload']['type'] == 'output_chunk']
        check('ANSI output cleaned', all('\x1b' not in c for c in chunks))
        result_box = []
        streamer = threading.Thread(target=lambda: result_box.append(command("printf 'STREAM_BEGIN\\n'; sleep 4; printf 'STREAM_END\\n'")))
        streamer.start()
        def stream_started():
            return any(e['payload'].get('type') == 'output_chunk' and 'STREAM_BEGIN' in e['payload'].get('text', '') for e in tool('read_output', limit=500)['events'])
        wait_for(stream_started, 3)
        check('MCP output before command completion', streamer.is_alive())
        streamer.join()
        check('stream completion', succeeded(result_box[0]))
        check('readonly bypass rejected', tool('run_readonly_command', shell='system', command='touch /tmp/not-readonly').get('isError'))
        shell = tool('open_shell', shell='system')['shell_id']
        check('persistent shell assignment', succeeded(command('export REMOTEOPS_FIXTURE=state_kept; cd /tmp', shell_id=shell)))
        check('persistent shell state', succeeded(command('test "$REMOTEOPS_FIXTURE" = state_kept && test "$PWD" = /tmp', shell_id=shell)))
        check('persistent shell close', succeeded(tool('close_shell', shell_id=shell)))
        shell = tool('open_shell', shell='system')['shell_id']
        command('exit', shell_id=shell)
        check('persistent shell exit', not succeeded(command('echo invalid', shell_id=shell)))
        for name in ['list_processes', 'list_services']:
            value = tool(name)
            check(name+' structured inventory', succeeded(value) and value['details']['returned'] == len(value['details']['items']) and value['details']['returned'] > 0)
        check('system information', succeeded(command('uname -a; cat /etc/os-release; id')))
        check('journalctl available', succeeded(command('journalctl --user -n 5 --no-pager')))
        payload = os.urandom(2*1024*1024) + '中文文件'.encode()
        (fixture/'mcp-files/input.bin').write_bytes(payload)
        check('nested upload', succeeded(tool('upload_file', local_path='input.bin', remote_path='nested/data.bin', overwrite=False)))
        check('overwrite denied', not succeeded(tool('upload_file', local_path='input.bin', remote_path='nested/data.bin', overwrite=False)))
        check('download', succeeded(tool('download_file', remote_path='nested/data.bin', local_path='out.bin', overwrite_local=False)))
        check('transfer SHA256 round trip', hashlib.sha256(payload).digest() == hashlib.sha256((fixture/'mcp-files/out.bin').read_bytes()).digest())
        (fixture/'agent-files/outside-link').symlink_to(fixture/'config.json')
        check('symlink escape denied', not succeeded(tool('download_file', remote_path='outside-link', local_path='escape-link', overwrite_local=False)))
        check('local overwrite denied', not succeeded(tool('download_file', remote_path='nested/data.bin', local_path='out.bin', overwrite_local=False)))
        check('overwrite failure preserves original', (fixture/'mcp-files/out.bin').read_bytes() == payload)
        check('traversal denied', not succeeded(tool('download_file', remote_path='../config.json', local_path='escape', overwrite_local=False)))
        check('move file', succeeded(tool('move_file', source_path='nested/data.bin', destination_path='nested/moved.bin', overwrite=False)))
        check('delete file', succeeded(tool('delete_file', remote_path='nested/moved.bin')))
        child = spawn(['/bin/sleep', '300'], 'child')
        check('terminate owned process', succeeded(tool('terminate_process', process_id=child.pid)))
        child.wait(timeout=5)
        check('PID1 rejected', not succeeded(tool('terminate_process', process_id=1)))
        check('systemd mutation denied without root', not succeeded(tool('control_service', service_name='remoteops-e2e-nonexistent.service', action='start')))
        check('open port probe', succeeded(tool('test_port', host='127.0.0.1', port=relay_port)))
        echo = socket.socket(); echo.bind(('127.0.0.1', 0)); echo.listen()
        def echo_once():
            conn, _ = echo.accept()
            with conn:
                conn.sendall(conn.recv(128))
            echo.close()
        threading.Thread(target=echo_once, daemon=True).start()
        tcp = tool('tcp_exchange', host='127.0.0.1', port=echo.getsockname()[1], data_base64=base64.b64encode(b'linux-e2e').decode(), timeout_millis=1000)
        check('TCP exchange', succeeded(tcp) and tcp.get('sha256') == hashlib.sha256(b'linux-e2e').hexdigest())
        master, slave = pty.openpty()
        serial = tool('open_serial', port_name=os.ttyname(slave), baud_rate=9600, writable=True)
        check('serial PTY open', succeeded(serial))
        serial_id = serial['details']['serial_session_id']
        check('serial PTY write', succeeded(tool('write_serial', serial_session_id=serial_id, data_base64=base64.b64encode(b'SERIAL_TEST').decode())))
        os.set_blocking(master, False)
        check('serial PTY bytes verified', wait_for(lambda: os.read(master, 100)) == b'SERIAL_TEST')
        check('serial PTY close', succeeded(tool('close_serial', serial_session_id=serial_id)))
        os.close(master); os.close(slave)
        ssh_result = tool('run_ssh', host='-invalid', port=22, username='test', command='true', readonly=True)
        check('SSH destination injection rejected', ssh_result.get('isError'))
        cursor = max(e['sequence'] for e in tool('read_output', limit=500)['events'])
        stop(agent)
        check('SIGTERM graceful cleanup', agent.returncode == 0 and not status.exists())
        agent = spawn(agent_args, 'agent')
        restarted = wait_for(paired_status)
        check('identity and pairing restored', restarted['agent_instance_id'] == initial['agent_instance_id'] and restarted['pairing_code'] == initial['pairing_code'])
        wait_for(lambda: succeeded(command('true')))
        check('existing MCP session restored', True)
        command('echo AFTER_AGENT_RESTART')
        check('incremental output after Agent restart', 'AFTER_AGENT_RESTART' in json.dumps(tool('read_output', after_sequence=cursor, limit=500)))
        link.drop()
        wait_for(lambda: succeeded(command('true')))
        check('short network interruption reconnect', True)
        sentinel = fixture/'mutation-count'
        def interrupt_mutation():
            wait_for(sentinel.exists)
            link.drop()
        threading.Thread(target=interrupt_mutation, daemon=True).start()
        command(f'echo once >> {sentinel}; sleep 3')
        wait_for(lambda: succeeded(command('true')))
        check('in-flight mutation not replayed', sentinel.read_text().splitlines() == ['once'])
        stop(relay)
        time.sleep(2)
        relay = spawn(relay_args, 'relay')
        wait_for(lambda: succeeded(command('true')), 60)
        check('isolated Relay restart reconnect', True)
        link.listener.close()
        link.drop()
        stop(agent, signal.SIGINT)
        check('SIGINT graceful cleanup', agent.returncode == 0 and not status.exists())
        for name in ['agent.log', 'mcp.log']:
            text = (fixture/name).read_text()
            check(name+' secret redaction', token not in text and initial['pairing_code'] not in text and 'PRIVATE KEY' not in text)
        check('audit exists', (fixture/'audit.jsonl').stat().st_size > 0)
    except Exception as exc:
        # No tool payloads, tokens or fixture state in the published failure record.
        results.append({'test': 'execution', 'status': 'failed', 'reason': type(exc).__name__ + ': ' + str(exc)[:160]})
        raise
    finally:
        for p in reversed(processes):
            try:
                stop(p)
            except subprocess.TimeoutExpired:
                p.kill(); p.wait()
        for f in logs:
            f.close()
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps({'platform': 'linux-x86_64', 'tests': results}, indent=2)+'\n')


if __name__ == '__main__':
    main()
