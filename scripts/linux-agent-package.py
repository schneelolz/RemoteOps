#!/usr/bin/env python3
"""Create source-derived legal notices and a verified binary release manifest."""
import hashlib
import json
import pathlib
import re
import subprocess
import sys

out = pathlib.Path(sys.argv[1])
meta = json.loads(subprocess.check_output(['cargo', 'metadata', '--locked', '--format-version', '1', '--filter-platform', 'x86_64-unknown-linux-gnu']))
packages = {p['id']: p for p in meta['packages']}
nodes = {p['id']: p for p in meta['resolve']['nodes']}
roots = [p['id'] for p in meta['packages'] if p['name'] in ('remoteops-agent', 'remoteops-agent-service')]
seen, todo = set(), list(roots)
while todo:
    key = todo.pop()
    if key in seen:
        continue
    seen.add(key)
    todo.extend(nodes[key]['dependencies'])
deps, notices = [], ['RemoteOps third-party licenses (Cargo.lock, Linux Agent dependency closure)\n']
for p in sorted((packages[i] for i in seen if i not in meta['workspace_members']), key=lambda p: (p['name'], p['version'])):
    if not p['license'] and not p.get('license_file'):
        raise SystemExit('Missing license: ' + p['name'])
    deps.append({k: p.get(k) for k in ['name', 'version', 'license', 'repository', 'source']})
    notices.append(f"\n----- {p['name']} {p['version']} | {p['license']} -----\n")
    for path in sorted(pathlib.Path(p['manifest_path']).parent.iterdir()):
        if path.is_file() and re.match(r'(?i)^(license|copying|notice|copyright)([._-].*)?$', path.name):
            notices.append(f'\n{path.name}\n' + path.read_text(errors='replace'))
(out / 'DEPENDENCIES.json').write_text(json.dumps(deps, indent=2) + '\n')
(out / 'THIRD_PARTY_LICENSES.txt').write_text('\n'.join(notices))
manifest = []
for name in ['remoteops-agent', 'remoteops-agent-service']:
    p = out / name
    data = p.read_bytes()
    assert data[:4] == b'\x7fELF', 'Expected a Linux ELF binary'
    assert int.from_bytes(data[18:20], 'little') == 62, 'Expected x86_64 ELF'
    version = subprocess.check_output([str(p), '--version'], text=True).strip().split()[-1]
    expected = next(packages[i]['version'] for i in roots if packages[i]['name'] == name)
    assert version == expected
    linkage = subprocess.run(['ldd', str(p)], text=True, capture_output=True)
    assert 'not found' not in linkage.stdout + linkage.stderr
    manifest.append({'File': name, 'CargoVersion': version, 'Target': 'x86_64-unknown-linux-gnu', 'Length': len(data), 'Sha256': hashlib.sha256(data).hexdigest()})
(out / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
