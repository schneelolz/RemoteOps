"""在已固定主机密钥的 Relay SSH 入口执行，仅更新管理前端容器。"""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time
import urllib.request

ROOT = Path('/opt/remoteops')
COMPOSE = ROOT / 'release/deploy/relay'
DATA = Path('/var/lib/docker/volumes/remoteops-relay-data/_data')
VERSION = '0.1.1'
BATCH = 'admin011-20261107'
STAGE = ROOT / 'staging' / BATCH
BACKUP = ROOT / 'backups' / ('remoteops-before-' + BATCH + '-verified')
IMAGE = 'remoteops-relay-admin:0.1.1-20261107'
BACKUP_RUNTIME = 'python@sha256:1e1f6334529d1fb26a2ef619d7cffb30a7c7d7cc1cc7338ebee283a58d1aeaca'

# 使用独立运行时支持 STRICT 表和 SQLite 在线备份，源卷只读挂载。
DATABASE_BACKUP = '''
from pathlib import Path
import json, os, sqlite3
os.umask(0o077)
source_root = Path('/source')
destination_root = Path('/backup')
databases = []
for path in source_root.rglob('*.sqlite3'):
    target = destination_root / path.relative_to(source_root)
    target.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    with sqlite3.connect(path.as_uri() + '?mode=ro', uri=True) as source, sqlite3.connect(target) as destination:
        source.backup(destination)
        assert destination.execute('pragma integrity_check').fetchone()[0] == 'ok'
    target.chmod(0o600)
    databases.append(path.relative_to(source_root).as_posix())
print(json.dumps(databases))
'''


def run(args):
    return subprocess.check_output(args, stderr=subprocess.PIPE).decode().strip()


def inspect(name):
    return json.loads(run(['docker', 'inspect', name]))[0]


def compose(*args):
    return run(['docker', 'compose', '--project-directory', str(COMPOSE), '-f', str(COMPOSE / 'docker-compose.yml'), *args])


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2))
    path.chmod(0o600)


def healthy(name):
    for _ in range(50):
        info = inspect(name)
        if info['State'].get('Health', {}).get('Status') == 'healthy':
            return info
        if info['State']['Status'] in ('exited', 'dead') or info['RestartCount']:
            raise RuntimeError('frontend health failed')
        time.sleep(1)
    raise TimeoutError('frontend health timeout')


def get(url, token=None):
    headers = {'Authorization': 'Bearer ' + token} if token else {}
    with urllib.request.urlopen(urllib.request.Request(url, headers=headers), timeout=15) as response:
        return response.read()


def backend_identity(info):
    return {key: value for key, value in [('id', info['Id']), ('image', info['Image']), ('started', info['State']['StartedAt']), ('restart_count', info['RestartCount'])]}


def catalog(info, port=18081):
    environment = dict(value.split('=', 1) for value in info['Config']['Env'])
    data = json.loads(get(f'http://127.0.0.1:{port}/api/admin/tools', environment['REMOTEOPS_ADMIN_TOKEN']))['data']
    content = {key: data.get(key) for key in ['revision', 'enabled', 'tools', 'guide_markdown']}
    return hashlib.sha256(json.dumps(content, sort_keys=True, separators=(',', ':')).encode()).hexdigest()


def verify_files(port):
    checked = 0
    for path in (STAGE / 'relay-admin/dist').rglob('*'):
        if path.is_file() and '.vite' not in path.parts:
            actual = get(f'http://127.0.0.1:{port}/' + path.relative_to(STAGE / 'relay-admin/dist').as_posix())
            assert hashlib.sha256(actual).digest() == hashlib.sha256(path.read_bytes()).digest()
            checked += 1
    return checked


def stage():
    manifest = json.loads((STAGE / 'stage-manifest.json').read_text())
    for filename, expected in manifest.items():
        assert hashlib.sha256((STAGE / filename).read_bytes()).hexdigest() == expected
    old_backend = backend_identity(healthy('remoteops-relay'))
    run(['docker', 'build', '--network', 'none', '--pull=false', '-t', IMAGE, '-f', str(STAGE / 'deploy/relay-admin/Dockerfile'), str(STAGE)])
    run(['docker', 'run', '--rm', '--network', 'none', '--read-only', '--tmpfs', '/tmp', IMAGE, '-t'])
    frontend = inspect('remoteops-relay-admin')
    networks = list(frontend['NetworkSettings']['Networks'])
    assert len(networks) == 1
    candidate = 'remoteops-admin011-check-' + str(int(time.time()))
    run(['docker', 'run', '-d', '--name', candidate, '--network', networks[0], '--read-only', '--tmpfs', '/tmp', '--cap-drop', 'ALL', '--security-opt', 'no-new-privileges:true', '-p', '127.0.0.1::8080', IMAGE])
    try:
        info = healthy(candidate)
        port = int(info['NetworkSettings']['Ports']['8080/tcp'][0]['HostPort'])
        release = json.loads(get(f'http://127.0.0.1:{port}/release.json'))
        assert release['version'] == VERSION
        count = verify_files(port)
        assert catalog(inspect('remoteops-relay'), port) == catalog(inspect('remoteops-relay'))
    finally:
        run(['docker', 'stop', candidate])
    assert backend_identity(inspect('remoteops-relay')) == old_backend
    result = {'image': IMAGE, 'image_id': inspect(IMAGE)['Id'], 'candidate_static_files_verified': count, 'same_origin_api_verified': True, 'backend_untouched': True}
    write_json(STAGE / 'prepared.json', result)
    print(json.dumps(result))


def rollback():
    for filename in ['.env', 'docker-compose.yml']:
        shutil.copy2(BACKUP / filename, COMPOSE / filename)
    compose('config', '--quiet')
    compose('up', '-d', '--no-build', '--no-deps', '--force-recreate', 'remoteops-relay-admin')
    healthy('remoteops-relay-admin')


def deploy():
    prepared = json.loads((STAGE / 'prepared.json').read_text())
    before_backend = backend_identity(healthy('remoteops-relay'))
    old_frontend = healthy('remoteops-relay-admin')
    before_catalog = catalog(inspect('remoteops-relay'))
    assert not BACKUP.exists()
    BACKUP.mkdir(mode=0o700)
    for filename in ['.env', 'docker-compose.yml']:
        shutil.copy2(COMPOSE / filename, BACKUP / filename)
        (BACKUP / filename).chmod(0o600)
    shutil.copy2(DATA / 'relay-state.json', BACKUP / 'relay-state.json')
    (BACKUP / 'relay-state.json').chmod(0o600)
    database_root = BACKUP / 'databases'
    database_root.mkdir(mode=0o700)
    metadata = DATA.stat()
    os.chown(database_root, metadata.st_uid, metadata.st_gid)
    databases = json.loads(subprocess.check_output([
        'docker', 'run', '--rm', '--network', 'none', '--read-only', '--cap-drop', 'ALL',
        '--security-opt', 'no-new-privileges:true', '--user', str(metadata.st_uid) + ':' + str(metadata.st_gid),
        '--mount', 'type=bind,src=' + str(DATA) + ',dst=/source,readonly',
        '--mount', 'type=bind,src=' + str(database_root) + ',dst=/backup',
        '-i', BACKUP_RUNTIME, 'python', '-'
    ], input=DATABASE_BACKUP.encode(), stderr=subprocess.PIPE))
    write_json(BACKUP / 'before.json', {'backend': before_backend, 'frontend_image': old_frontend['Image'], 'frontend_tag': old_frontend['Config']['Image'], 'catalog_sha256': before_catalog, 'online_database_backups': databases})
    shutil.copy2(__file__, BACKUP / 'rollback.py')
    switched = False
    try:
        envpath = COMPOSE / '.env'
        lines = envpath.read_text().splitlines()
        indexes = [i for i, line in enumerate(lines) if line.startswith('REMOTEOPS_RELAY_ADMIN_IMAGE=')]
        assert len(indexes) <= 1
        if indexes:
            lines[indexes[0]] = 'REMOTEOPS_RELAY_ADMIN_IMAGE=' + IMAGE
        else:
            lines.append('REMOTEOPS_RELAY_ADMIN_IMAGE=' + IMAGE)
        temporary = COMPOSE / '.env.admin011-new'
        temporary.write_text('\n'.join(lines) + '\n')
        temporary.chmod(0o600)
        temporary.replace(envpath)
        switched = True
        compose('config', '--quiet')
        compose('up', '-d', '--no-build', '--no-deps', '--force-recreate', 'remoteops-relay-admin')
        frontend = healthy('remoteops-relay-admin')
        assert frontend['Image'] == prepared['image_id']
        assert frontend['Config']['User'] == old_frontend['Config']['User']
        assert frontend['HostConfig']['PortBindings'] == old_frontend['HostConfig']['PortBindings']
        assert backend_identity(healthy('remoteops-relay')) == before_backend
        assert catalog(inspect('remoteops-relay')) == before_catalog
        count = verify_files(18081)
        result = {'frontend_version': VERSION, 'frontend_health': 'healthy', 'frontend_restarts': frontend['RestartCount'], 'backend_identity_unchanged': True, 'backend_restarted': False, 'catalog_unchanged': True, 'static_files_verified': count, 'backup': str(BACKUP), 'online_database_backups': len(databases)}
        write_json(BACKUP / 'deployment.json', result)
        print(json.dumps(result))
    except Exception:
        logs = subprocess.run(['docker', 'logs', '--tail', '200', 'remoteops-relay-admin'], stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        (BACKUP / 'failed-frontend.log').write_bytes(logs.stdout)
        if switched:
            rollback()
        raise


if __name__ == '__main__':
    os.umask(0o077)
    try:
        {'stage': stage, 'deploy': deploy, 'rollback': rollback}[sys.argv[1]]()
    except Exception as error:
        print(json.dumps({'failed_stage': sys.argv[1], 'error_type': type(error).__name__, 'evidence': str(BACKUP)}))
        sys.exit(1)
