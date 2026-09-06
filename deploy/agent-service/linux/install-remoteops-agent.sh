#!/usr/bin/env bash
set -euo pipefail
[[ $EUID == 0 ]] || { echo 'Run this installer as root.' >&2; exit 1; }
package_dir=$(cd "$(dirname "$0")" && pwd)
config_source=${1:-}
for file in remoteops-agent remoteops-agent-service remoteops-agent.service; do
  [[ -f "$package_dir/$file" ]] || { echo "Missing package file: $file" >&2; exit 1; }
done
if [[ -n "$config_source" ]]; then
  config_source=$(realpath "$config_source")
  python3 - "$config_source" <<'PY'
import json, sys
with open(sys.argv[1]) as f:
    c=json.load(f)
if not c.get('relay') or 'example.com' in c['relay']:
    raise SystemExit('Set a real Relay address in your configuration.')
PY
elif [[ ! -f /etc/remoteops/agent-config.json ]]; then
  echo 'First install requires a configured JSON file: install-remoteops-agent.sh /path/to/config.json' >&2
  exit 1
fi
if ! id remoteops >/dev/null 2>&1; then
  useradd --system --user-group --home-dir /var/lib/remoteops --shell /usr/sbin/nologin remoteops
fi
install -d -m 0755 /usr/lib/remoteops
install -d -m 0750 -o root -g remoteops /etc/remoteops
install -d -m 0700 -o remoteops -g remoteops /var/lib/remoteops /var/lib/remoteops/transfers
if [[ -n "$config_source" && "$config_source" != /etc/remoteops/agent-config.json ]]; then
  if [[ -f /etc/remoteops/agent-config.json ]]; then
    cp -p /etc/remoteops/agent-config.json "/etc/remoteops/agent-config.json.backup-$(date +%Y%m%d%H%M%S)"
  fi
  install -m 0640 -o root -g remoteops "$config_source" /etc/remoteops/agent-config.json
fi
# Validate as the actual runtime account before disrupting an existing service.
install -m 0755 "$package_dir/remoteops-agent-service" /usr/lib/remoteops/remoteops-agent-service.new
runuser -u remoteops -- /usr/lib/remoteops/remoteops-agent-service.new --config /etc/remoteops/agent-config.json --check-config
systemctl stop remoteops-agent.service 2>/dev/null || true
for file in remoteops-agent remoteops-agent-service; do
  install -m 0755 "$package_dir/$file" "/usr/lib/remoteops/$file.new"
  mv -f "/usr/lib/remoteops/$file.new" "/usr/lib/remoteops/$file"
done
install -m 0644 "$package_dir/remoteops-agent.service" /etc/systemd/system/remoteops-agent.service
systemctl daemon-reload
systemctl enable --now remoteops-agent.service
systemctl is-active --quiet remoteops-agent.service
echo 'RemoteOps Agent installed. Inspect status-remoteops-agent.sh for Relay connectivity.'
