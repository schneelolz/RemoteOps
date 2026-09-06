#!/usr/bin/env bash
set -euo pipefail
[[ $EUID == 0 ]] || { echo 'Run as root.' >&2; exit 1; }
systemctl disable --now remoteops-agent.service 2>/dev/null || true
python3 - <<'PY'
from pathlib import Path
for filename in ['/etc/systemd/system/remoteops-agent.service', '/usr/lib/remoteops/remoteops-agent', '/usr/lib/remoteops/remoteops-agent-service']:
    Path(filename).unlink(missing_ok=True)
PY
systemctl daemon-reload
systemctl reset-failed remoteops-agent.service 2>/dev/null || true
echo 'Removed Agent binaries and service. Configuration, account and identity state preserved.'
