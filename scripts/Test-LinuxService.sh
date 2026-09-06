#!/usr/bin/env bash
# Dedicated Ubuntu test VM only: exercises installed service and preserves data.
set -euo pipefail
[[ $EUID == 0 ]] || { echo 'Requires root on the dedicated test VM.' >&2; exit 1; }
package=$(realpath "${1:?Pass extracted Linux package directory}")
evidence=${2:-/var/lib/remoteops-acceptance}
install -d -m 0700 "$evidence"
exec > >(tee "$evidence/systemd-tests.log") 2>&1
check() { "$@"; echo "PASS: $*"; }
(cd "$package"; sha256sum -c SHA256SUMS >/dev/null)
echo 'PASS: release SHA256 manifest'
config_hash=$(sha256sum /etc/remoteops/agent-config.json | cut -d' ' -f1)
systemctl stop remoteops-agent.service
install -d -m 0700 /var/backups/remoteops
tar -czf /var/backups/remoteops/pre-acceptance.tar.gz -C / etc/remoteops var/lib/remoteops
chmod 0600 /var/backups/remoteops/pre-acceptance.tar.gz
check bash "$package/install-remoteops-agent.sh"
check systemctl is-enabled --quiet remoteops-agent.service
check systemctl is-active --quiet remoteops-agent.service
wait_relay() {
  for ((i=0;i<40;i++)); do
    if python3 - <<'PY'
import json
from pathlib import Path
p=Path('/run/remoteops-agent/runtime-status.json')
try:
    c=json.loads(p.read_text())
    assert c['status'] in ('waiting', 'controlled')
    assert p.stat().st_mode & 0o777 == 0o600
except (OSError, ValueError, AssertionError):
    raise SystemExit(1)
PY
    then echo 'PASS: Relay connected and private runtime status'; return; fi
    sleep 1
  done
  return 1
}
wait_relay
main_pid=$(systemctl show -p MainPID --value remoteops-agent.service)
check test "$(ps -o user= -p "$main_pid" | xargs)" = remoteops
check systemctl kill --signal=KILL --kill-whom=main remoteops-agent.service
sleep 7
check systemctl is-active --quiet remoteops-agent.service
check test "$(systemctl show -p MainPID --value remoteops-agent.service)" != "$main_pid"
wait_relay
check systemctl stop remoteops-agent.service
check test "$(systemctl show -p ExecMainStatus --value remoteops-agent.service)" = 0
check test ! -e /run/remoteops-agent/runtime-status.json
check bash "$package/uninstall-remoteops-agent.sh"
check test ! -e /etc/systemd/system/remoteops-agent.service
check test ! -e /usr/lib/remoteops/remoteops-agent-service
check test -e /var/lib/remoteops/agent-state.json
check bash "$package/install-remoteops-agent.sh"
wait_relay
check test "$(sha256sum /etc/remoteops/agent-config.json | cut -d' ' -f1)" = "$config_hash"
check bash "$package/status-remoteops-agent.sh"
# Restrict the positive service-control fixture to exactly one unit and account.
cat > /etc/systemd/system/remoteops-acceptance-fixture.service <<'UNIT'
[Unit]
Description=RemoteOps acceptance fixture
[Service]
Type=simple
User=remoteops
ExecStart=/bin/sleep infinity
UNIT
cat > /etc/polkit-1/rules.d/49-remoteops-acceptance.rules <<'RULE'
polkit.addRule(function(action, subject) {
  if (subject.user === 'remoteops' && action.id === 'org.freedesktop.systemd1.manage-units' && action.lookup('unit') === 'remoteops-acceptance-fixture.service') {
    return polkit.Result.YES;
  }
});
RULE
chmod 0644 /etc/systemd/system/remoteops-acceptance-fixture.service /etc/polkit-1/rules.d/49-remoteops-acceptance.rules
systemctl daemon-reload
echo 'PASS: lifecycle complete; isolated service-control fixture ready'
