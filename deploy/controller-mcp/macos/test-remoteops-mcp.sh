#!/bin/bash
set -euo pipefail

EXPECTED_MCP_VERSION="0.2.0-preview.13"
EXPECTED_CREDENTIAL_PROMPT_VERSION="0.2.0-preview.5"
KEYCHAIN_SERVICE="RemoteOps Controller Token"
CURRENT_USER="$(id -un)"
CODEX_HOME="${CODEX_HOME:-$HOME/.codex}"
SKIP_NETWORK=0
if [[ "${1:-}" == "--skip-network" ]]; then SKIP_NETWORK=1; fi

failures=0
pass() { echo "[通过] $1"; }
fail() {
  echo "[失败] $1" >&2
  failures=$((failures + 1))
}

[[ "$(uname -s)" == "Darwin" ]] || fail "当前系统不是 macOS。"
[[ "$(uname -m)" == "arm64" ]] || fail "当前 Mac 不是 Apple Silicon（arm64）。"

CONFIG_PATH="$CODEX_HOME/config.toml"
CONNECTION_CONFIG="$CODEX_HOME/remoteops/controller-config.json"
LAUNCHER="$CODEX_HOME/remoteops/launch-remoteops-controller-mcp.sh"
BINARY="$CODEX_HOME/remoteops/remoteops-controller-mcp-$EXPECTED_MCP_VERSION"
CREDENTIAL_PROMPT="$CODEX_HOME/remoteops/remoteops-credential-prompt"

[[ -x "$BINARY" ]] && pass "MCP 程序存在且可执行。" || fail "未找到当前版本 MCP：$BINARY"
[[ -x "$CREDENTIAL_PROMPT" ]] && pass "SSH 密码安全输入程序存在且可执行。" || fail "缺少 SSH 密码安全输入程序。"
if [[ -x "$BINARY" ]]; then
  version_output="$("$BINARY" --version 2>&1 || true)"
  [[ "$version_output" == *"$EXPECTED_MCP_VERSION"* ]] && pass "MCP 版本：$version_output" || fail "MCP 版本不正确：$version_output"
fi
if [[ -x "$CREDENTIAL_PROMPT" ]]; then
  prompt_version_output="$("$CREDENTIAL_PROMPT" --version 2>&1 || true)"
  [[ "$prompt_version_output" == *"$EXPECTED_CREDENTIAL_PROMPT_VERSION"* ]] && pass "SSH 密码安全输入程序版本：$prompt_version_output" || fail "SSH 密码安全输入程序版本不正确：$prompt_version_output"
fi
credential_id="$(plutil -extract credential_id raw "$CONNECTION_CONFIG" 2>/dev/null || true)"
MCP_COMMAND="$LAUNCHER"
if [[ -n "$credential_id" ]]; then
  MCP_COMMAND="$BINARY"
  if "$BINARY" --check-credential --config "$CONNECTION_CONFIG" >/dev/null; then
    pass "独立 Controller 凭据存在于 macOS Keychain（未读取到脚本）。"
  else
    fail "无法读取此安装的 Keychain 凭据；请保留设置状态并重新运行安装器。"
  fi
else
  [[ -x "$LAUNCHER" ]] && pass "Keychain 启动脚本存在。" || fail "缺少 MCP 启动脚本。"
  security find-generic-password -a "$CURRENT_USER" -s "$KEYCHAIN_SERVICE" -w >/dev/null 2>&1 && pass "Controller Token 已存在于 Keychain。" || fail "Keychain 中没有 Controller Token。"
fi

INSPECTION="$(mktemp "${TMPDIR:-/tmp}/remoteops-inspection.XXXXXX")"
trap 'rm -f "$INSPECTION"' EXIT
if [[ -f "$CONFIG_PATH" && -x "$BINARY" ]] && "$BINARY" --inspect-codex "$CONFIG_PATH" > "$INSPECTION"; then
  configured_command="$(plutil -extract command raw "$INSPECTION" 2>/dev/null || true)"
  configured_config="$(plutil -extract args_config raw "$INSPECTION" 2>/dev/null || true)"
  if [[ "$configured_command" == "$MCP_COMMAND" && "$configured_config" == "$CONNECTION_CONFIG" ]]; then
    pass "Codex MCP 配置已指向当前安装的程序与连接配置。"
  else
    fail "Codex remoteops 路径或参数不匹配；不会执行配置中的未知命令。"
  fi
  tool_timeout="$(plutil -extract tool_timeout_sec raw "$INSPECTION" 2>/dev/null || true)"
  if [[ "$tool_timeout" =~ ^[0-9]+$ && "$tool_timeout" -ge 360 ]]; then
    pass "RemoteOps MCP 工具超时至少为 360 秒。"
  else
    fail "RemoteOps MCP tool_timeout_sec 必须至少为 360 秒。"
  fi
  approval_mode="$(plutil -extract default_tools_approval_mode raw "$INSPECTION" 2>/dev/null || true)"
  control_mode_approval="$(plutil -extract set_control_mode_approval_mode raw "$INSPECTION" 2>/dev/null || true)"
  if [[ "$approval_mode" == 'approve' && "$control_mode_approval" == 'prompt' ]]; then
    pass "RemoteOps 工具审批与 set_control_mode 独立确认已配置；全局 Codex 策略由用户管理。"
  else
    fail "RemoteOps MCP 工具审批或 set_control_mode 确认配置不完整。"
  fi
  legacy_env="$(plutil -extract legacy_env raw "$INSPECTION" 2>/dev/null || true)"
  if [[ "$legacy_env" != 'false' ]]; then
    fail "macOS 配置不应转发旧 Controller 环境变量。"
  fi
else
  fail "无法解析 Codex remoteops 配置。"
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
  host="${host#\[}"
  host="${host%\]}"
  if [[ -n "$host" && "$port" =~ ^[0-9]+$ ]] && nc -G 5 -z "$host" "$port" >/dev/null 2>&1; then
    pass "$relay TCP 可达。"
  else
    fail "无法连接 Relay：$relay"
  fi
fi

if [[ "$failures" -ne 0 ]]; then exit 1; fi
echo
echo "RemoteOps MCP 安装检查通过。完全重启 Codex 后输入 /mcp 查看 remoteops。"
