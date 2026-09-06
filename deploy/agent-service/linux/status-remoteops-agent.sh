#!/usr/bin/env bash
set -euo pipefail
status_file=${REMOTEOPS_STATUS_FILE:-/run/remoteops-agent/runtime-status.json}
python3 - "$status_file" "${1:-}" <<'PY'
import datetime, json, sys
try:
    with open(sys.argv[1]) as f:
        data=json.load(f)
except (OSError, ValueError) as e:
    raise SystemExit('Runtime status unavailable; run as root or the service account: '+str(e))
if sys.argv[2]=='--pairing':
    expires=data.get('pairing_code_expires_at')
    if not data.get('pairing_code') or not expires or datetime.datetime.fromisoformat(expires.replace('Z','+00:00')) <= datetime.datetime.now(datetime.timezone.utc):
        raise SystemExit('No valid pairing code; the Agent may be offline.')
    print(data['pairing_code'])
else:
    print(json.dumps({k:data.get(k) for k in ['status','relay','active_connections']},ensure_ascii=False,indent=2))
PY
