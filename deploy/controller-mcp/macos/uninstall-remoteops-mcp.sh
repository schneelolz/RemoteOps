#!/bin/bash
set -euo pipefail

CODEX_HOME="${CODEX_HOME:-$HOME/.codex}"
KEYCHAIN_SERVICE="RemoteOps Controller Token"
CURRENT_USER="$(id -un)"
CONFIG_PATH="$CODEX_HOME/config.toml"
CONNECTION_CONFIG="$CODEX_HOME/remoteops/controller-config.json"
BINARY="$CODEX_HOME/remoteops/remoteops-controller-mcp-0.2.0-preview.13"

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "此卸载脚本仅支持 macOS。" >&2
  exit 1
fi

if [[ -f "$CONFIG_PATH" ]]; then
  if [[ ! -x "$BINARY" ]]; then
    echo "缺少当前 MCP 程序，无法安全更改 Codex 配置。请恢复程序后再卸载。" >&2
    exit 1
  fi
  "$BINARY" --validate-codex "$CONFIG_PATH"
fi

# Remove the per-installation credential before losing its non-secret reference.
credential_id="$(plutil -extract credential_id raw "$CONNECTION_CONFIG" 2>/dev/null || true)"
if [[ -n "$credential_id" ]]; then
  if [[ ! -x "$BINARY" ]]; then
    echo "缺少当前 MCP 程序，无法安全删除独立 Keychain 凭据。请恢复程序后再卸载。" >&2
    exit 1
  fi
  "$BINARY" --remove-credential --config "$CONNECTION_CONFIG"
fi

if [[ -f "$CONFIG_PATH" ]]; then
  "$BINARY" --unconfigure-codex "$CONFIG_PATH"
fi

rm -f "$CODEX_HOME/remoteops"/remoteops-controller-mcp-*
rm -f "$CODEX_HOME/remoteops/remoteops-credential-prompt"
rm -f "$CODEX_HOME/remoteops/launch-remoteops-controller-mcp.sh"
rm -f "$CODEX_HOME/remoteops/controller-config.json"
rm -f "$CODEX_HOME/remoteops/setup-state.json"
rm -f "$CODEX_HOME/remoteops/relay-ca.pem"
rm -rf "$HOME/.agents/skills/remoteops" "$CODEX_HOME/skills/remoteops"
if [[ -z "$credential_id" ]]; then
  security delete-generic-password -a "$CURRENT_USER" -s "$KEYCHAIN_SERVICE" >/dev/null 2>&1 || true
fi

echo "RemoteOps MCP 已卸载。审计日志和文件交换目录仍保留。"
