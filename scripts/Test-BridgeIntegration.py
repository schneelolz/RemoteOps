"""仅使用回环 Relay 和临时身份验证 Bridge；不访问生产端点，不输出控制码。"""
import argparse
import concurrent.futures
import hashlib
import itertools
import json
import os
from pathlib import Path
import queue
import re
import secrets
import socket
import statistics
import ssl
import subprocess
import tempfile
import threading
import time
import uuid


def address():
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        return f'127.0.0.1:{sock.getsockname()[1]}'


class Rpc:
    def __init__(self, command, env, cwd, handler=None):
        self.process = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=subprocess.DEVNULL, env=env, cwd=cwd,
                                        creationflags=subprocess.CREATE_NO_WINDOW if os.name == 'nt' else 0)
        self.messages = queue.Queue(maxsize=128)
        self.pending = {}
        self.lock = threading.Lock()
        self.sequence = 0
        self.handler = handler
        self.ready = threading.Event()
        self.code = None
        self.pool = concurrent.futures.ThreadPoolExecutor(max_workers=16)
        threading.Thread(target=self.read, daemon=True).start()

    def send(self, message):
        data = (json.dumps(message, ensure_ascii=False) + '\n').encode('utf-8')
        with self.lock:
            self.process.stdin.write(data)
            self.process.stdin.flush()

    def read(self):
        try:
            while line := self.process.stdout.readline(4 * 1024 * 1024 + 1):
                if len(line) > 4 * 1024 * 1024:
                    raise RuntimeError('oversized_response')
                message = json.loads(line)
                if message.get('method') == 'host.invoke':
                    self.pool.submit(self.invoke, message)
                elif message.get('method') == 'bridge.status' and message.get('params', {}).get('state') == 'ready':
                    self.code = message['params']['accessCode']
                    self.ready.set()
                elif 'method' not in message:
                    completion = self.pending.get(str(message.get('id')))
                    if completion:
                        completion.put(message)
        except (OSError, ValueError):
            pass

    def invoke(self, message):
        try:
            result = self.handler(message['params'])
            self.send(dict(jsonrpc='2.0', id=message['id'], result=result))
        except Exception:
            self.send(dict(jsonrpc='2.0', id=message['id'], error=dict(code=-32000, message='host_rejected')))

    def request(self, method, params, timeout=40):
        with self.lock:
            self.sequence += 1
            identifier = str(self.sequence)
            completion = queue.Queue(maxsize=1)
            self.pending[identifier] = completion
        try:
            self.send(dict(jsonrpc='2.0', id=identifier, method=method, params=params))
            result = completion.get(timeout=timeout)
            if 'error' in result:
                raise RuntimeError('rpc_request_rejected:' + method)
            return result['result']
        finally:
            self.pending.pop(identifier, None)

    def close(self):
        try:
            self.process.stdin.close()
            self.process.wait(timeout=15)
        except (OSError, subprocess.TimeoutExpired):
            self.process.kill()
            self.process.wait()
        self.pool.shutdown(wait=True)


def structured(result):
    if result.get('isError'):
        reason = re.sub(r'[0-9a-fA-F]{8}-[0-9a-fA-F-]{27}|\b\d{3}-?\d{3}-?\d{3}\b', '<redacted>', json.dumps(result, ensure_ascii=False))
        raise RuntimeError('mcp_tool_rejected:' + reason[:1200])
    if 'structuredContent' in result:
        return result['structuredContent']
    return json.loads(next(item['text'] for item in result['content'] if item['type'] == 'text'))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--skip-build', action='store_true')
    parser.add_argument('--binary-directory', type=Path)
    parser.add_argument('--bridge-executable', type=Path)
    parser.add_argument('--csharp-configuration', choices=('Debug', 'Release'), default='Release')
    args = parser.parse_args()
    repo = Path(__file__).resolve().parent.parent
    env = {key: value for key, value in os.environ.items() if not key.startswith('REMOTEOPS_')}
    if not args.skip_build:
        subprocess.run(['cargo', 'build', '--locked', '-p', 'remoteops-bridge', '-p', 'remoteops-relay', '-p', 'remoteops-controller-mcp'], cwd=repo, check=True, env=env)
    def binary(name):
        if name == 'RemoteOpsBridge' and args.bridge_executable:
            return str(args.bridge_executable.resolve())
        return str((args.binary_directory or repo/'target/debug') / (name + ('.exe' if os.name == 'nt' else '')))
    report = dict(result='in_progress', coexistence=False, business=False, capability_denial=False, csharp={}, startup={})
    with tempfile.TemporaryDirectory(prefix='remoteops-bridge-') as temporary:
        root = Path(temporary)
        endpoint = address()
        env.update(REMOTEOPS_HUMAN_CONTROLLER_TOKEN=secrets.token_hex(32), REMOTEOPS_AI_CONTROLLER_TOKEN=secrets.token_hex(32), REMOTEOPS_CONTROLLER_OWNER_ID=str(uuid.uuid4()))
        relay = subprocess.Popen([binary('remoteops-relay'), '--bind', endpoint, '--health-bind', address(), '--tls-cert', str(root/'cert.pem'), '--tls-key', str(root/'key.pem'), '--state-file', str(root/'relay-state.json')], cwd=root, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        children = []
        try:
            for _ in range(150):
                if relay.poll() is not None:
                    raise RuntimeError('relay_start_failed')
                if (root/'cert.pem').exists():
                    try:
                        with socket.create_connection(('127.0.0.1', int(endpoint.split(':')[1])), timeout=.1):
                            break
                    except OSError:
                        pass
                time.sleep(.1)
            else:
                raise TimeoutError('relay_start_timeout')

            def configuration(role, instance):
                return dict(protocolVersion=1, role=role, instanceId=instance, relay=endpoint, serverName='127.0.0.1', dataRoot=str(root/'data'), caCert=str(root/'cert.pem'), allowedCapabilities=['host_business'])

            invocation_counts = {}
            def start(role, instance):
                config = configuration(role, instance)
                config['host'] = dict(serviceId=role, serviceVersion='test-1', instanceId=instance, protocolVersion=1, capabilities=[dict(id='user.current', version='1', description='模拟只读用户查询', readOnly=True, inputSchema=dict(type='object', additionalProperties=False), outputSchema=dict(type='object'))])
                def handler(request):
                    if request['instanceId'] != instance or request['capabilityId'] != 'user.current' or request['version'] != '1' or request['arguments'] != {}:
                        raise RuntimeError('invalid_host_query')
                    invocation_counts[instance] = invocation_counts.get(instance, 0) + 1
                    if instance == 'timeout-test':
                        time.sleep(31)
                    return dict(name='模拟用户', role=role)
                bridge = Rpc([binary('RemoteOpsBridge')], env, root, handler)
                children.append(bridge)
                started = time.monotonic()
                bridge.request('initialize', config)
                if not bridge.ready.wait(20):
                    raise TimeoutError('bridge_not_ready')
                assert uuid.UUID(bridge.code).version == 4
                return bridge, round((time.monotonic() - started) * 1000, 2)

            first, _ = start('grclient', 'client-a')
            second, _ = start('udserver', 'server-a')
            assert first.code != second.code
            mcp_env = dict(env, REMOTEOPS_CONTROLLER_TOKEN=env['REMOTEOPS_AI_CONTROLLER_TOKEN'])
            (root/'mcp.json').write_text('{}', encoding='utf-8')
            mcp = Rpc([binary('remoteops-controller-mcp'), '--config', str(root/'mcp.json'), '--relay', endpoint, '--server-name', '127.0.0.1', '--ca-cert', str(root/'cert.pem'), '--audit-log', str(root/'audit.jsonl'), '--transfer-root', str(root/'controller-transfer')], mcp_env, root)
            children.append(mcp)
            mcp.request('initialize', dict(protocolVersion='2025-11-25', capabilities={}, clientInfo=dict(name='bridge-test', version='1')))
            mcp.send(dict(jsonrpc='2.0', method='notifications/initialized'))
            def tool(name, arguments):
                return structured(mcp.request('tools/call', dict(name=name, arguments=arguments)))
            def pair_when_ready(code):
                deadline = time.monotonic() + 10
                while True:
                    result = mcp.request('tools/call', dict(name='pair_connection', arguments=dict(pairing_code=code)))
                    if not result.get('isError'):
                        return structured(result)
                    if '离线' not in json.dumps(result, ensure_ascii=False) or time.monotonic() >= deadline:
                        return structured(result)
                    time.sleep(.1)
            sessions = []
            for bridge, instance in ((first, 'client-a'), (second, 'server-a')):
                paired = pair_when_ready(bridge.code)
                session = paired['session_id']
                sessions.append(session)
                description = tool('describe_host_capabilities', dict(session_id=session))
                assert description['details']['instanceId'] == instance
                result = tool('query_host_capability', dict(session_id=session, instance_id=instance, capability_id='user.current', version='1', arguments={}))
                assert result['details']['name'] == '模拟用户'
            assert len(set(sessions)) == 2
            report.update(coexistence=True, business=True)
            (root/'ordinary-config.json').write_text('{}', encoding='utf-8')
            ordinary = subprocess.Popen([binary('remoteops-agent'), '--config', str(root/'ordinary-config.json'), '--relay', endpoint, '--server-name', '127.0.0.1', '--ca-cert', str(root/'cert.pem'), '--state-file', str(root/'ordinary-state.json'), '--transfer-root', str(root/'ordinary-transfer')], env=env, cwd=root, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, creationflags=subprocess.CREATE_NO_WINDOW if os.name == 'nt' else 0)
            try:
                deadline = time.monotonic() + 20
                short = None
                while time.monotonic() < deadline:
                    try:
                        state = json.loads((root/'relay-state.json').read_text(encoding='utf-8-sig'))
                    except (OSError, ValueError):
                        time.sleep(.05)
                        continue
                    short = next((record['lease']['pairing_code'] for record in state['agents'] if record['hello']['pairing_mode'] == 'auto_per_boot'), None)
                    if short:
                        break
                    time.sleep(.1)
                assert short and len(short) == 9
                third = pair_when_ready(short)
                assert third['session_id'] not in sessions
                unsupported = mcp.request('tools/call', dict(name='describe_host_capabilities', arguments=dict(session_id=third['session_id'])))
                assert 'host_business_unsupported' in json.dumps(unsupported)
                assert first.request('status', {})['state'] == 'ready'
                assert second.request('status', {})['state'] == 'ready'
                report['ordinary_agent_plus_two_bridges'] = True
            finally:
                ordinary.terminate()
                ordinary.wait(timeout=10)
            denied = mcp.request('tools/call', dict(name='run_readonly_command', arguments=dict(session_id=sessions[0], shell='cmd', command='ipconfig')))
            report['capability_denial'] = bool(denied.get('isError')) or 'product_policy_disabled' in json.dumps(denied)
            assert report['capability_denial']

            for mode in ('cold', 'warm'):
                samples = []
                for number in range(5):
                    instance = f'perf-{number}' if mode == 'cold' else 'perf-warm'
                    bridge, elapsed = start('grclient', instance)
                    samples.append(elapsed)
                    bridge.request('shutdown', {})
                    bridge.close()
                    children.remove(bridge)
                report['startup'][mode] = dict(samples_ms=samples, median_ms=statistics.median(samples), slowest_ms=max(samples), definition='全新实例目录' if mode == 'cold' else '复用实例身份目录的新进程')

            slow, _ = start('grclient', 'timeout-test')
            slow_session = pair_when_ready(slow.code)['session_id']
            timeout_result = mcp.request('tools/call', dict(name='query_host_capability', arguments=dict(session_id=slow_session, instance_id='timeout-test', capability_id='user.current', version='1', arguments={})), timeout=40)
            encoded = json.dumps(timeout_result, ensure_ascii=False)
            assert 'timeout' in encoded.lower() or '超时' in encoded
            assert invocation_counts.get('timeout-test') == 1
            slow.request('shutdown', {})
            slow.close()
            children.remove(slow)
            report['host_timeout_no_replay'] = True

            for framework, role in itertools.product(('net462', 'net10.0'), ('grclient', 'udserver')):
                config = configuration(role, 'csharp-' + framework.replace('.', '') + '-' + role)
                config['allowedCapabilities'].append('cmd')
                config_path = root / (framework + '-' + role + '.json')
                config_path.write_text(json.dumps(config), encoding='utf-8')
                host = repo/'examples/bridge-csharp-host/bin'/args.csharp_configuration/framework/('BridgeHost.exe' if framework == 'net462' else 'BridgeHost.dll')
                command = [str(host)] if framework == 'net462' else ['dotnet', str(host)]
                host_process = subprocess.Popen(command + [binary('RemoteOpsBridge'), role, str(config_path)], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env, cwd=root, creationflags=subprocess.CREATE_NO_WINDOW if os.name == 'nt' else 0)
                try:
                    # 宿主保持运行，使中文登记消息和实际子进程完成握手。
                    deadline = time.monotonic() + 20
                    records = []
                    while time.monotonic() < deadline:
                        if host_process.poll() is not None:
                            raise RuntimeError('csharp_host_exited:' + framework)
                        try:
                            state = json.loads((root/'relay-state.json').read_text(encoding='utf-8-sig'))
                            identity = next((root/'data'/role/config['instanceId']).glob('*/identity-*.json'))
                            agent_id = json.loads(identity.read_text())['agent_instance_id']
                            records = [record for record in state['agents'] if record['hello']['agent_instance_id'] == agent_id]
                            if records:
                                break
                        except (OSError, ValueError, StopIteration):
                            pass
                        time.sleep(.1)
                    if not records:
                        stdout, stderr = host_process.communicate(b'\n', timeout=20)
                        raise RuntimeError('csharp_registration_timeout:' + framework + ':' + stdout.decode('utf-8', errors='replace')[-1000:] + stderr.decode('utf-8', errors='replace')[-1000:])
                    paired = pair_when_ready(records[0]['lease']['pairing_code'])
                    session = paired['session_id']
                    capability = 'client.currentUser' if role == 'grclient' else 'store.summary'
                    result = tool('query_host_capability', dict(session_id=session, instance_id=config['instanceId'], capability_id=capability, version='1', arguments={}))
                    assert result['details']['simulated'] is True
                    ips = tool('query_host_capability', dict(session_id=session, instance_id=config['instanceId'], capability_id='system.ipAddresses', version='1', arguments={}))
                    assert isinstance(ips['details']['addresses'], list) and ips['details']['addresses']
                    shell = tool('run_readonly_command', dict(session_id=session, shell='cmd', command='ipconfig'))
                    assert shell['exit_code'] == 0
                    report['csharp'][framework + '-' + role] = '实际双向模拟业务、宿主 IP 与 RemoteOps CMD IP 查询通过'
                    host_process.communicate(b'\n', timeout=20)
                    assert host_process.returncode == 0
                finally:
                    if host_process.poll() is None:
                        try:
                            host_process.communicate(b'\n', timeout=20)
                        except subprocess.TimeoutExpired:
                            host_process.kill()
                            host_process.wait()

            # 同一实例第二进程不得替换已有连接。
            duplicate = Rpc([binary('RemoteOpsBridge')], env, root)
            children.append(duplicate)
            duplicate.request('initialize', configuration('grclient', 'client-a'))
            time.sleep(.5)
            assert not duplicate.ready.is_set()
            assert first.request('status', {})['state'] == 'ready'
            report['duplicate_instance_rejected'] = True
            report['certificate_failures'] = {}
            for case in ('wrong-name', 'wrong-pin', 'untrusted'):
                config = configuration('grclient', 'certificate-' + case)
                if case == 'wrong-name':
                    config['serverName'] = 'not-the-relay.invalid'
                else:
                    config['caCert'] = None
                if case == 'wrong-pin':
                    config['tlsFingerprint'] = '0' * 64
                bridge = Rpc([binary('RemoteOpsBridge')], env, root)
                children.append(bridge)
                bridge.request('initialize', config)
                deadline = time.monotonic() + 8
                status = {}
                while time.monotonic() < deadline:
                    status = bridge.request('status', {})
                    if status.get('code') == 'tls_trust_failed':
                        break
                    time.sleep(.05)
                assert status.get('code') == 'tls_trust_failed' and not bridge.ready.is_set(), 'certificate_rejection_missing:' + case
                bridge.request('shutdown', {})
                bridge.close()
                children.remove(bridge)
                report['certificate_failures'][case] = '结构化拒绝，无弹窗'
            # 用独立回环 Relay 验证已固定且匹配的证书过期时仍会拒绝。
            if os.name == 'nt':
                certificate_env = dict(env, BRIDGE_CERT_TEST_ROOT=str(root))
                subprocess.run(['pwsh', '-NoProfile', '-Command', '''
$ErrorActionPreference = 'Stop'
$rsa = [Security.Cryptography.RSA]::Create(2048)
$request = [Security.Cryptography.X509Certificates.CertificateRequest]::new('CN=127.0.0.1', $rsa, [Security.Cryptography.HashAlgorithmName]::SHA256, [Security.Cryptography.RSASignaturePadding]::Pkcs1)
$san = [Security.Cryptography.X509Certificates.SubjectAlternativeNameBuilder]::new()
$san.AddIpAddress([Net.IPAddress]::Loopback)
$request.CertificateExtensions.Add($san.Build())
$certificate = $request.CreateSelfSigned([DateTimeOffset]::UtcNow.AddDays(-2), [DateTimeOffset]::UtcNow.AddDays(-1))
[IO.File]::WriteAllText((Join-Path $env:BRIDGE_CERT_TEST_ROOT 'expired-cert.pem'), $certificate.ExportCertificatePem())
[IO.File]::WriteAllText((Join-Path $env:BRIDGE_CERT_TEST_ROOT 'expired-key.pem'), $rsa.ExportPkcs8PrivateKeyPem())
$certificate.Dispose()
$rsa.Dispose()
'''], env=certificate_env, check=True, capture_output=True)
                expired_endpoint = address()
                expired_relay = subprocess.Popen([binary('remoteops-relay'), '--bind', expired_endpoint, '--health-bind', address(), '--tls-cert', str(root/'expired-cert.pem'), '--tls-key', str(root/'expired-key.pem'), '--state-file', str(root/'expired-state.json')], cwd=root, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                try:
                    for _ in range(100):
                        if expired_relay.poll() is not None:
                            raise RuntimeError('expired_certificate_relay_failed')
                        try:
                            with socket.create_connection(('127.0.0.1', int(expired_endpoint.split(':')[1])), timeout=.1):
                                break
                        except OSError:
                            time.sleep(.05)
                    config = configuration('grclient', 'certificate-expired')
                    config.update(relay=expired_endpoint, caCert=None, tlsFingerprint=hashlib.sha256(ssl.PEM_cert_to_DER_cert((root/'expired-cert.pem').read_text())).hexdigest())
                    bridge = Rpc([binary('RemoteOpsBridge')], env, root)
                    children.append(bridge)
                    bridge.request('initialize', config)
                    deadline = time.monotonic() + 8
                    status = {}
                    while time.monotonic() < deadline:
                        status = bridge.request('status', {})
                        if status.get('code') == 'tls_trust_failed':
                            break
                        time.sleep(.05)
                    assert status.get('code') == 'tls_trust_failed' and not bridge.ready.is_set()
                    bridge.request('shutdown', {})
                    bridge.close()
                    children.remove(bridge)
                    report['certificate_failures']['expired-matching-pin'] = '匹配指纹仍拒绝过期证书，无弹窗'
                finally:
                    expired_relay.terminate()
                    expired_relay.wait(timeout=10)
            # 本机协议异常帧与消息上限，不触发任何远程操作。
            malformed = subprocess.Popen([binary('RemoteOpsBridge')], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, env=env, cwd=root, creationflags=subprocess.CREATE_NO_WINDOW if os.name == 'nt' else 0)
            stdout, _ = malformed.communicate(b'not-json\n', timeout=15)
            assert json.loads(stdout)['error']['code'] == -32600
            oversized = subprocess.Popen([binary('RemoteOpsBridge')], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, env=env, cwd=root, creationflags=subprocess.CREATE_NO_WINDOW if os.name == 'nt' else 0)
            oversized.communicate(b'x' * (256 * 1024 + 1) + b'\n', timeout=15)
            assert oversized.returncode != 0
            report['invalid_and_oversized_messages'] = True
            report['result'] = 'passed'
        finally:
            for child in reversed(children):
                child.close()
            relay.terminate()
            relay.wait(timeout=10)
            # Windows 下等待被终止的测试 Agent 所启动的短暂环境探测子进程释放 cwd。
            time.sleep(1)
    output = repo/'artifacts/acceptance/bridge-integration.json'
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding='utf-8')
    print(json.dumps(report, ensure_ascii=False, indent=2))


if __name__ == '__main__':
    main()
