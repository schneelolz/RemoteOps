#!/bin/bash
set -euo pipefail

EXPECTED_VERSION="0.2.0-preview.1"
KEYCHAIN_SERVICE="RemoteOps Controller Token"
CURRENT_USER="$(id -un)"
CODEX_HOME="${CODEX_HOME:-$HOME/.codex}"
SKIP_NETWORK=0
if [[ "${1:-}" == "--skip-network" ]]; then SKIP_NETWORK=1; fi

failures=0
pass() { echo "[通过] $1"; }
fail() { echo "[失败] $1" >&2; failures=$((failures + 1)); }

[[ "$(uname -s)" == "Darwin" ]] || fail "当前系统不是 macOS。"
[[ "$(uname -m)" == "arm64" ]] || fail "当前 Mac 不是 Apple Silicon（arm64）。"

CONFIG_PATH="$CODEX_HOME/config.toml"
CONNECTION_CONFIG="$CODEX_HOME/remoteops/controller-config.json"
LAUNCHER="$CODEX_HOME/remoteops/launch-remoteops-controller-mcp.sh"
BINARY="$CODEX_HOME/remoteops/remoteops-controller-mcp-$EXPECTED_VERSION"

[[ -x "$BINARY" ]] && pass "MCP 程序存在且可执行。" || fail "未找到当前版本 MCP：$BINARY"
if [[ -x "$BINARY" ]]; then
  version_output="$($BINARY --version 2>&1 || true)"
  [[ "$version_output" == *"$EXPECTED_VERSION"* ]] && pass "MCP 版本：$version_output" || fail "MCP 版本不正确：$version_output"
fi
[[ -x "$LAUNCHER" ]] && pass "Keychain 启动脚本存在。" || fail "缺少 MCP 启动脚本。"
security find-generic-password -a "$CURRENT_USER" -s "$KEYCHAIN_SERVICE" -w >/dev/null 2>&1 && pass "Controller Token 已存在于 Keychain。" || fail "Keychain 中没有 Controller Token。"

if [[ -f "$CONFIG_PATH" ]] && grep -Fq '[mcp_servers.remoteops]' "$CONFIG_PATH" && grep -Fq "$LAUNCHER" "$CONFIG_PATH"; then
  pass "Codex MCP 配置已指向当前启动脚本。"
else
  fail "Codex config.toml 未正确配置 remoteops。"
fi
if [[ -f "$CONFIG_PATH" ]] && grep -Fq 'default_tools_approval_mode = "approve"' "$CONFIG_PATH" && grep -Eq '^approval_policy[[:space:]]*=[[:space:]]*\{[[:space:]]*granular[[:space:]]*=[[:space:]]*\{[^}]*mcp_elicitations[[:space:]]*=[[:space:]]*true' "$CONFIG_PATH"; then
  pass "RemoteOps MCP 交互确认已启用，且不会重复触发 Codex 静态工具审批。"
else
  fail "RemoteOps MCP 交互确认配置不完整。"
fi
if [[ -f "$CONFIG_PATH" ]] && grep -Fq '[mcp_servers.remoteops.tools.set_control_mode]' "$CONFIG_PATH" && grep -Fq 'approval_mode = "prompt"' "$CONFIG_PATH"; then
  pass "完全控制仅通过 set_control_mode 的 Codex 工具确认启用。"
else
  fail "set_control_mode 未配置独立 Codex 工具确认。"
fi
if [[ -f "$CONNECTION_CONFIG" ]] && plutil -lint "$CONNECTION_CONFIG" >/dev/null; then
  pass "RemoteOps 连接配置是有效 JSON。"
  owner_id="$(plutil -extract owner_id raw "$CONNECTION_CONFIG" 2>/dev/null || true)"
  if [[ "$owner_id" =~ ^[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}$ && "$owner_id" != "00000000-0000-0000-0000-000000000000" ]]; then
    pass "统一 Controller Owner 已配置（不会显示其值）。"
  else
    fail "连接配置缺少有效的非空 Owner UUID。"
  fi
else
  fail "RemoteOps 连接配置缺失或无效。"
fi
[[ -f "$HOME/.agents/skills/remoteops/SKILL.md" ]] && pass "标准 RemoteOps skill 已安装。" || fail "标准 RemoteOps skill 未安装。"
[[ -f "$CODEX_HOME/skills/remoteops/SKILL.md" ]] && pass "兼容 RemoteOps skill 已安装。" || fail "兼容 RemoteOps skill 未安装。"

if [[ "$SKIP_NETWORK" -eq 0 && -f "$CONNECTION_CONFIG" ]]; then
  relay="$(plutil -extract relay raw "$CONNECTION_CONFIG" 2>/dev/null || true)"
  host="${relay%:*}"
  port="${relay##*:}"
  host="${host#\[}"; host="${host%\]}"
  if [[ -n "$host" && "$port" =~ ^[0-9]+$ ]] && nc -G 5 -z "$host" "$port" >/dev/null 2>&1; then
    pass "$relay TCP 可达。"
  else
    fail "无法连接 Relay：$relay"
  fi
fi

if [[ "$failures" -ne 0 ]]; then exit 1; fi
echo
echo "RemoteOps MCP 安装检查通过。完全重启 Codex 后输入 /mcp 查看 remoteops。"
