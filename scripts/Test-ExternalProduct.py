#!/usr/bin/env python3
"""Build a genuine standalone consumer outside the RemoteOps workspace."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile


def run(args, cwd, env, capture=False):
    return subprocess.run(args, cwd=cwd, env=env, check=True, text=True, encoding="utf-8",
                          stdout=subprocess.PIPE if capture else None).stdout


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--update-lock', action='store_true')
    parser.add_argument('--output', default='artifacts/acceptance/external-product.json')
    args = parser.parse_args()
    repo = Path(__file__).resolve().parent.parent
    env = {k: v for k, v in os.environ.items() if not k.startswith(('REMOTEOPS_', 'EXAMPLE_ASSIST_'))}
    env['CARGO_TARGET_DIR'] = str(repo / 'target')
    with tempfile.TemporaryDirectory(prefix='remoteops-external-') as directory:
        root = Path(directory).resolve() / 'example-assist'
        assert not root.is_relative_to(repo)
        shutil.copytree(repo / 'examples/example-assist', root, ignore=shutil.ignore_patterns('target'))
        manifest = root / 'Cargo.toml'
        text = manifest.read_text()
        for relative in ('apps/remoteops-agent-gui', 'crates/remoteops-build'):
            text = text.replace('../../' + relative, (repo / relative).as_posix())
        manifest.write_text(text)
        if args.update_lock:
            run(['cargo', 'generate-lockfile'], root, env)
            shutil.copy2(root / 'Cargo.lock', repo / 'examples/example-assist/Cargo.lock')
        for action in ('check', 'test', 'build'):
            run(['cargo', action, '--locked'], root, env)
        metadata = json.loads(run(['cargo', 'metadata', '--format-version', '1', '--locked'], root, env, True))
        assert Path(metadata['workspace_root']).resolve() == root
        for name in ('remoteops-agent-gui', 'remoteops-agent', 'remoteops-protocol', 'remoteops-domain', 'egui'):
            assert sum(p['name'] == name for p in metadata['packages']) == 1, name
        binary = repo / 'target/debug' / ('example-assist.exe' if os.name == 'nt' else 'example-assist')
        info = json.loads(run([str(binary), '--build-info'], root, env, True))
        config = root / 'config.json'
        config.write_text('{}')
        check = [str(binary), '--check-config', '--demo', '--config', str(config)]
        run(check, root, env)
        for key in ('RELAY', 'SERVER_NAME'):
            hostile = dict(env, **{'EXAMPLE_ASSIST_' + key: 'other.example.invalid:7443' if key == 'RELAY' else 'other.example.invalid'})
            result = subprocess.run(check, cwd=root, env=hostile, capture_output=True)
            assert result.returncode != 0, f'fixed policy did not reject {key}'
        native = 'not applicable on this platform'
        if os.name == 'nt':
            script = '''$ErrorActionPreference='Stop'
$v = (Get-Item -LiteralPath $env:EXAMPLE_BINARY).VersionInfo
if ($v.ProductName -ne 'Example Assist' -or $v.FileDescription -ne 'Example Assist field assistance' -or $v.ProductVersion -ne '1.0.0') { throw 'Incorrect product resources' }
Add-Type -AssemblyName System.Drawing
$icon = [Drawing.Icon]::ExtractAssociatedIcon($env:EXAMPLE_BINARY)
if ($null -eq $icon) { throw 'Missing executable icon' }
$bitmap = $icon.ToBitmap()
$pixel = $bitmap.GetPixel([int]($bitmap.Width / 8), [int]($bitmap.Height / 8))
if ($pixel.R -ne 24 -or $pixel.G -ne 103 -or $pixel.B -ne 125) { throw 'Wrong product icon' }
$bitmap.Dispose()
$icon.Dispose()
'''
            run(['pwsh', '-NoProfile', '-Command', script], root, dict(env, EXAMPLE_BINARY=str(binary)))
            native = 'passed'
        report = {'external_workspace': str(root), 'build_info': info,
                  'sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
                  'windows_resources': native, 'result': 'passed'}
        output = Path(args.output)
        if not output.is_absolute(): output = repo / output
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(json.dumps(report, indent=2) + '\n')
        print(json.dumps(report, indent=2))


if __name__ == '__main__':
    main()
