#!/bin/bash
set -euo pipefail

CODEX_HOME="${CODEX_HOME:-$HOME/.codex}"
KEYCHAIN_SERVICE="RemoteOps Controller Token"
CURRENT_USER="$(id -un)"
CONFIG_PATH="$CODEX_HOME/config.toml"

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "此卸载脚本仅支持 macOS。" >&2
  exit 1
fi

if [[ -f "$CONFIG_PATH" ]]; then
  cp "$CONFIG_PATH" "$CONFIG_PATH.remoteops-backup-$(date +%Y%m%d-%H%M%S)"
  temp_config="$(mktemp "${TMPDIR:-/tmp}/remoteops-config.XXXXXX")"
  awk '
    /^\[mcp_servers\.remoteops(\.|\])/{ skip=1; next }
    /^\[/ { if (skip) skip=0 }
    !skip { print }
  ' "$CONFIG_PATH" > "$temp_config"
  mv "$temp_config" "$CONFIG_PATH"
fi

rm -f "$CODEX_HOME/remoteops"/remoteops-controller-mcp-*
rm -f "$CODEX_HOME/remoteops/launch-remoteops-controller-mcp.sh"
rm -f "$CODEX_HOME/remoteops/controller-config.json"
rm -f "$CODEX_HOME/remoteops/relay-ca.pem"
rm -rf "$HOME/.agents/skills/remoteops" "$CODEX_HOME/skills/remoteops"
security delete-generic-password -a "$CURRENT_USER" -s "$KEYCHAIN_SERVICE" >/dev/null 2>&1 || true

echo "RemoteOps MCP 已卸载。审计日志和文件交换目录仍保留。"
