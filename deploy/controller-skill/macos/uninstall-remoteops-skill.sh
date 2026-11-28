#!/bin/bash
set -euo pipefail
if [[ "$(uname -s)" != Darwin ]]; then
  echo '此入口仅支持 macOS。Windows 请使用 Uninstall-RemoteOpsSkill.ps1。' >&2
  exit 1
fi
command -v python3 >/dev/null || { echo '需要 Python 3；未更改任何文件。' >&2; exit 1; }
python3 -c 'import sys; sys.exit(0 if sys.version_info >= (3, 9) else 1)' || { echo '需要 Python 3.9 或更新版本；未更改任何文件。' >&2; exit 1; }
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd -P)"
exec python3 "$SCRIPT_DIR/manage-remoteops-skill.py" uninstall "$@"
