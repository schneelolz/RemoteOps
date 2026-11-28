"""仅在已授权的 Relay 主机上执行；不输出配置、凭据或节点标识。"""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time
import urllib.request

import yaml

ROOT = Path('/opt/remoteops')
COMPOSE = ROOT / 'release/deploy/relay'
DATA = Path('/var/lib/docker/volumes/remoteops-relay-data/_data')
BATCH = 'preview17-admin010-20261106'
STAGE = ROOT / 'staging' / BATCH
BACKUP = ROOT / 'backups' / ('remoteops-before-' + BATCH)
BACKEND = 'remoteops-relay:0.2.0-preview.17-p20-admin010-20261106'
FRONTEND = 'remoteops-relay-admin:0.1.0-20261106'
CHECK = 'remoteops-relay-admin010-copy-check'


def run(args):
    return subprocess.check_output(args, stderr=subprocess.PIPE).decode().strip()


def inspect(name='remoteops-relay'):
    return json.loads(run(['docker', 'inspect', name]))[0]


def compose(*args):
    return run(['docker', 'compose', '--project-directory', str(COMPOSE), '-f', str(COMPOSE / 'docker-compose.yml'), *args])


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2))
    path.chmod(0o600)


def env(info):
    return dict(value.split('=', 1) for value in info['Config']['Env'])


def request(url, token=None):
    headers = {'Authorization': 'Bearer ' + token} if token else {}
    with urllib.request.urlopen(urllib.request.Request(url, headers=headers), timeout=15) as response:
        return response.read()


def catalog(info, port=18081):
    data = json.loads(request(f'http://127.0.0.1:{port}/api/admin/tools', env(info)['REMOTEOPS_ADMIN_TOKEN']))['data']
    digest = lambda value: hashlib.sha256(json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(',', ':')).encode()).hexdigest()
    return {'revision': data['revision'], 'enabled': data['enabled'], 'tool_count': len(data['tools']), 'tools_sha256': digest(data['tools']), 'guide_sha256': digest(data.get('guide_markdown', ''))}


def healthy(name):
    for _ in range(50):
        info = inspect(name)
        if info['State'].get('Health', {}).get('Status') == 'healthy':
            return info
        if info['State']['Status'] in ('exited', 'dead') or info.get('RestartCount', 0):
            raise RuntimeError('container health')
        time.sleep(1)
    raise TimeoutError('container health')


def db_facts(directory):
    return json.loads(run(['docker', 'run', '--rm', '--network', 'none', '--read-only', '--entrypoint', '/sqlite-check', '--mount', 'type=bind,src=' + str(directory) + ',dst=/data', '--mount', 'type=bind,src=' + str(STAGE / 'sqlite-check') + ',dst=/sqlite-check,readonly', BACKEND]))


def save_logs(name, path):
    result = subprocess.run(['docker', 'logs', '--tail', '300', name], stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    path.write_bytes(result.stdout)
    path.chmod(0o600)


def stage():
    expected = json.loads((STAGE / 'stage-manifest.json').read_text())
    for name, digest in expected.items():
        assert hashlib.sha256((STAGE / name).read_bytes()).hexdigest() == digest
    binary = STAGE / 'remoteops-relay'
    binary.chmod(0o755)
    old = healthy('remoteops-relay')
    base = 'remoteops-relay:admin010-base-' + old['Image'].split(':')[1][:12]
    run(['docker', 'tag', old['Image'], base])
    backend = STAGE / 'backend'
    backend.mkdir(exist_ok=True)
    shutil.copy2(binary, backend / binary.name)
    (backend / 'Dockerfile').write_text('FROM ' + base + '\nUSER root\nCOPY --chown=remoteops:remoteops remoteops-relay /usr/local/bin/remoteops-relay\nUSER remoteops\n')
    run(['docker', 'build', '--network', 'none', '--pull=false', '-t', BACKEND, str(backend)])
    run(['docker', 'build', '--network', 'none', '--pull=false', '-t', FRONTEND, '-f', str(STAGE / 'deploy/relay-admin/Dockerfile'), str(STAGE)])
    version = run(['docker', 'run', '--rm', '--network', 'none', BACKEND, '--version'])
    assert version == 'remoteops-relay 0.2.0-preview.17'
    run(['docker', 'run', '--rm', '--network', 'none', '--read-only', '--tmpfs', '/tmp', FRONTEND, '-t'])
    # 使用现有 API 只读验证独立前端代理，不替换生产服务。
    networks = list(old['NetworkSettings']['Networks'])
    assert len(networks) == 1
    preview = 'remoteops-relay-admin010-stage-check-' + str(int(time.time()))
    run(['docker', 'run', '-d', '--name', preview, '--network', networks[0], '--read-only', '--tmpfs', '/tmp', '--cap-drop', 'ALL', '--security-opt', 'no-new-privileges:true', '-p', '127.0.0.1::8080', FRONTEND])
    try:
        preview_info = healthy(preview)
        preview_port = int(preview_info['NetworkSettings']['Ports']['8080/tcp'][0]['HostPort'])
        assert b'/assets/' in request(f'http://127.0.0.1:{preview_port}/')
        assert catalog(old, preview_port) == catalog(old)
    finally:
        run(['docker', 'stop', preview])
    shutil.copy2(ROOT / 'staging/preview16-p20-jlai13-20261106/remoteops-sqlite-readonly-check', STAGE / 'sqlite-check')
    prepared = {'backend_image': BACKEND, 'frontend_image': FRONTEND, 'backend_id': inspect(BACKEND)['Id'], 'frontend_id': inspect(FRONTEND)['Id'], 'backend_version': version, 'old_image': old['Config']['Image'], 'static_sha256': expected['frontend.tar'], 'stage_readonly_proxy_passed': True}
    write_json(STAGE / 'prepared.json', prepared)
    print(json.dumps(prepared))


def rollback():
    assert BACKUP.parent.resolve() == ROOT / 'backups'
    # 本次不改变数据库结构；恢复原镜像及端口，保留当前数据和故障证据。
    try:
        compose('stop', 'remoteops-relay-admin')
    except subprocess.CalledProcessError:
        pass
    for name in ('docker-compose.yml', '.env'):
        shutil.copy2(BACKUP / name, COMPOSE / name)
    compose('config', '--quiet')
    compose('up', '-d', '--no-build', '--force-recreate', 'remoteops-relay')
    healthy('remoteops-relay')
    assert catalog(inspect()) == json.loads((BACKUP / 'catalog-before.json').read_text())
    print(json.dumps({'rollback': 'healthy', 'backup': str(BACKUP)}))


def deploy():
    prepared = json.loads((STAGE / 'prepared.json').read_text())
    old = healthy('remoteops-relay')
    assert shutil.disk_usage(ROOT).free > 2 * 1024**3
    assert not BACKUP.exists()
    BACKUP.mkdir(mode=0o700)
    for name in ('docker-compose.yml', '.env'):
        shutil.copy2(COMPOSE / name, BACKUP / name)
        (BACKUP / name).chmod(0o600)
    shutil.copy2(__file__, BACKUP / 'rollback.py')
    (BACKUP / 'runtime.env').write_text(''.join(k + '=' + v + '\n' for k, v in env(old).items()))
    (BACKUP / 'runtime.env').chmod(0o600)
    before = catalog(old)
    write_json(BACKUP / 'catalog-before.json', before)
    changed = False
    try:
        compose('stop', 'remoteops-relay')
        run(['tar', '--numeric-owner', '-cpf', str(BACKUP / 'data.tar'), '-C', str(DATA), '.'])
        (BACKUP / 'data.tar').chmod(0o600)
        run(['tar', '-tf', str(BACKUP / 'data.tar')])
        write_json(BACKUP / 'backup-complete.json', {'data_sha256': hashlib.sha256((BACKUP / 'data.tar').read_bytes()).hexdigest(), 'old_image': old['Image']})
        clone = BACKUP / 'compatibility-copy'
        clone.mkdir(mode=0o700)
        run(['tar', '--numeric-owner', '-xpf', str(BACKUP / 'data.tar'), '-C', str(clone)])
        meta = DATA.stat()
        os.chown(clone, meta.st_uid, meta.st_gid)
        clone.chmod(meta.st_mode & 0o777)
        databases = db_facts(clone)
        for database in clone.rglob('*.sqlite3'):
            metadata = database.stat()
            for sidecar in database.parent.glob(database.name + '-*'):
                os.chown(sidecar, metadata.st_uid, metadata.st_gid)
        run(['docker', 'run', '-d', '--name', CHECK, '--network', 'none', '--restart', 'no', '--env-file', str(BACKUP / 'runtime.env'), '--mount', 'type=bind,src=' + str(clone) + ',dst=/data', BACKEND])
        healthy(CHECK)
        run(['docker', 'stop', CHECK])
        save_logs(CHECK, BACKUP / 'compatibility.log')
        after_db = db_facts(clone)
        assert databases == after_db
        assert all(value['integrity'] == 'ok' for value in after_db.values())
        write_json(BACKUP / 'database-verification.json', {'before': databases, 'after': after_db, 'unchanged': True})
        document = yaml.safe_load((COMPOSE / 'docker-compose.yml').read_text())
        backend = document['services']['remoteops-relay']
        backend['image'] = '${REMOTEOPS_RELAY_IMAGE:-' + BACKEND + '}'
        old_ports = backend['ports']
        updated = []
        for value in old_ports:
            assert isinstance(value, str)
            if value.endswith(':18081'):
                assert value.startswith('127.0.0.1:')
                value = '127.0.0.1:18082:18081'
            updated.append(value)
        assert updated != old_ports
        backend['ports'] = updated
        backend.setdefault('networks', ['default'])
        document['services']['remoteops-relay-admin'] = {
            'image': '${REMOTEOPS_RELAY_ADMIN_IMAGE:-' + FRONTEND + '}', 'container_name': 'remoteops-relay-admin', 'restart': backend.get('restart', 'unless-stopped'), 'read_only': True,
            'tmpfs': ['/tmp:size=128m,mode=1777'], 'security_opt': ['no-new-privileges:true'], 'cap_drop': ['ALL'],
            'ports': ['127.0.0.1:${REMOTEOPS_RELAY_ADMIN_PORT:-18081}:8080'], 'networks': backend['networks'],
            'depends_on': {'remoteops-relay': {'condition': 'service_healthy'}}}
        temporary = COMPOSE / 'docker-compose.admin010-new.yml'
        temporary.write_text(yaml.safe_dump(document, allow_unicode=True, sort_keys=False))
        temporary.chmod(0o600)
        changed = True
        temporary.replace(COMPOSE / 'docker-compose.yml')
        envpath = COMPOSE / '.env'
        lines = envpath.read_text().splitlines()
        for key, value in [('REMOTEOPS_RELAY_IMAGE', BACKEND), ('REMOTEOPS_RELAY_ADMIN_IMAGE', FRONTEND)]:
            positions = [i for i, line in enumerate(lines) if line.startswith(key + '=')]
            assert len(positions) <= 1
            if positions:
                lines[positions[0]] = key + '=' + value
            else:
                lines.append(key + '=' + value)
        envnew = COMPOSE / '.env.admin010-new'
        envnew.write_text('\n'.join(lines) + '\n')
        envnew.chmod(0o600)
        envnew.replace(envpath)
        compose('config', '--quiet')
        compose('up', '-d', '--no-build', '--force-recreate', 'remoteops-relay', 'remoteops-relay-admin')
        current = healthy('remoteops-relay')
        front = healthy('remoteops-relay-admin')
        assert current['Image'] == prepared['backend_id'] and front['Image'] == prepared['frontend_id']
        assert current['Config']['User'] == old['Config']['User']
        assert env(current) == env(old)
        assert [(m.get('Name'), m['Destination']) for m in current['Mounts']] == [(m.get('Name'), m['Destination']) for m in old['Mounts']]
        expected_ports = dict(old['HostConfig']['PortBindings'])
        expected_ports['18081/tcp'] = [{'HostIp': '127.0.0.1', 'HostPort': '18082'}]
        assert current['HostConfig']['PortBindings'] == expected_ports
        assert request('http://127.0.0.1:18080/health').strip() == b'ok'
        assert catalog(current) == before
        assert b'/assets/' in request('http://127.0.0.1:18081/')
        release = json.loads(request('http://127.0.0.1:18081/release.json'))
        assert release['version'] == '0.1.0'
        assert run(['docker', 'exec', 'remoteops-relay', '/usr/local/bin/remoteops-relay', '--version']) == prepared['backend_version']
        result = {'status': 'healthy', 'backend': prepared['backend_version'], 'frontend': release['version'], 'backend_restarts': current['RestartCount'], 'frontend_restarts': front['RestartCount'], 'backup': str(BACKUP), 'catalog_preserved': True, 'database_unchanged': True, 'runtime_environment_preserved': True, 'volume_preserved': True, 'tls_and_health_ports_preserved': True, 'admin_entry_port': 18081, 'api_loopback_port': 18082, 'frontend_user': front['Config']['User'], 'frontend_readonly': front['HostConfig']['ReadonlyRootfs']}
        write_json(BACKUP / 'deployment-verification.json', result)
        print(json.dumps(result))
    except Exception:
        save_logs('remoteops-relay', BACKUP / 'failure-backend.log')
        try:
            save_logs('remoteops-relay-admin', BACKUP / 'failure-frontend.log')
        except Exception:
            pass
        if changed:
            rollback()
        else:
            compose('up', '-d', '--no-build', 'remoteops-relay')
            healthy('remoteops-relay')
        raise


if __name__ == '__main__':
    os.umask(0o077)
    try:
        {'stage': stage, 'deploy': deploy, 'rollback': rollback}[sys.argv[1]]()
    except Exception as error:
        print(json.dumps({'failed_stage': sys.argv[1], 'error_type': type(error).__name__, 'evidence': str(BACKUP)}))
        sys.exit(1)
