#!/bin/bash
set +x
set -euo pipefail
umask 077

MCP_PACKAGE_VERSION="0.2.0-preview.16"
CREDENTIAL_PROMPT_VERSION="0.2.0-preview.5"
CODEX_HOME="${CODEX_HOME:-$HOME/.codex}"
COMMAND_MODE="agent-controlled"
SETUP_FILE=""
SETUP_STDIN=0
SETUP_CODE=0
CONFIRM_ENROLLMENT=0
SETUP_MODE=0
setup_content=""
trap 'unset setup_content' EXIT

usage() {
  cat <<'EOF'
用法：
  ./install-remoteops-mcp.sh --setup-file <设置文件路径> [--confirm-enrollment]
  ./install-remoteops-mcp.sh --setup-code
  ./install-remoteops-mcp.sh --setup-stdin [--confirm-enrollment]

选项：
  --setup-file <路径>         导入管理员签发的一次性设置文件
  --setup-code                隐藏提示粘贴一次性设置码；不接受命令行密钥值
  --setup-stdin               从标准输入读取设置文件内容或设置码
  --confirm-enrollment        已确认设置文件中的登记地址，允许非交互登记
  --command-mode <模式>       readonly|approval|agent-controlled|full-access
  --codex-home <路径>         默认 ~/.codex
  -h, --help                  显示帮助
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --setup-file) SETUP_FILE="${2:?--setup-file 需要文件路径}"; SETUP_MODE=$((SETUP_MODE + 1)); shift 2 ;;
    --setup-code) SETUP_CODE=1; SETUP_MODE=$((SETUP_MODE + 1)); shift ;;
    --setup-stdin) SETUP_STDIN=1; SETUP_MODE=$((SETUP_MODE + 1)); shift ;;
    --confirm-enrollment) CONFIRM_ENROLLMENT=1; shift ;;
    --relay|--owner-id|--server-name|--ca-cert|--tls-fingerprint|--skip-token-prompt)
      echo "已停用 Token 手工安装；请使用 --setup-file、--setup-code 或 --setup-stdin。" >&2
      exit 1 ;;
    --command-mode) COMMAND_MODE="${2:-}"; shift 2 ;;
    --codex-home) CODEX_HOME="${2:-}"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "未知参数；一次性设置码只能通过隐藏提示、文件或标准输入提供。" >&2; usage >&2; exit 2 ;;
  esac
done

if [[ "$SETUP_MODE" -ne 1 ]]; then
  echo "安装必须选择 --setup-file、--setup-code 或 --setup-stdin；不再支持 Token 安装。" >&2
  exit 1
fi
if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "此安装包仅支持 macOS。" >&2
  exit 1
fi
if [[ "$(uname -m)" != "arm64" ]]; then
  echo "此安装包仅支持 Apple Silicon（arm64）Mac。" >&2
  exit 1
fi
case "$COMMAND_MODE" in
  readonly|approval|agent-controlled|full-access) ;;
  *) echo "--command-mode 无效。" >&2; exit 1 ;;
esac
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd -P)"
SOURCE_BINARY="$SCRIPT_DIR/remoteops-controller-mcp"
SOURCE_CREDENTIAL_PROMPT="$SCRIPT_DIR/remoteops-credential-prompt"
SOURCE_SKILL="$SCRIPT_DIR/skills/remoteops"
if [[ ! -x "$SOURCE_BINARY" ]]; then
  echo "安装包缺少可执行文件：$SOURCE_BINARY" >&2
  exit 1
fi
if [[ ! -x "$SOURCE_CREDENTIAL_PROMPT" ]]; then
  echo "安装包缺少可执行文件：$SOURCE_CREDENTIAL_PROMPT" >&2
  exit 1
fi
if [[ ! -f "$SOURCE_SKILL/SKILL.md" ]]; then
  echo "安装包缺少 RemoteOps skill。" >&2
  exit 1
fi
if ! "$SOURCE_BINARY" --version | grep -Fq "$MCP_PACKAGE_VERSION"; then
  echo "MCP 可执行文件版本与安装包不一致。" >&2
  exit 1
fi
if ! "$SOURCE_CREDENTIAL_PROMPT" --version | grep -Fq "$CREDENTIAL_PROMPT_VERSION"; then
  echo "SSH 密码安全输入程序版本与安装包不一致。" >&2
  exit 1
fi

CONFIG_PATH="$CODEX_HOME/config.toml"
# Validate before changing any installation files or credentials.
"$SOURCE_BINARY" --validate-codex "$CONFIG_PATH"

INSTALL_DIR="$CODEX_HOME/remoteops"
INSTALLED_BINARY="$INSTALL_DIR/remoteops-controller-mcp-$MCP_PACKAGE_VERSION"
INSTALLED_CREDENTIAL_PROMPT="$INSTALL_DIR/remoteops-credential-prompt"
CONNECTION_CONFIG="$INSTALL_DIR/controller-config.json"
CONFIG_PATH="$CODEX_HOME/config.toml"
STANDARD_SKILL_DIR="$HOME/.agents/skills/remoteops"
COMPAT_SKILL_DIR="$CODEX_HOME/skills/remoteops"
mkdir -p "$INSTALL_DIR" "$STANDARD_SKILL_DIR" "$COMPAT_SKILL_DIR"
install -m 755 "$SOURCE_BINARY" "$INSTALLED_BINARY"
install -m 755 "$SOURCE_CREDENTIAL_PROMPT" "$INSTALLED_CREDENTIAL_PROMPT"

if [[ -n "$SETUP_FILE" ]]; then
  [[ -f "$SETUP_FILE" ]] || { echo "找不到一次性设置文件。" >&2; exit 1; }
  setup_content="$(cat -- "$SETUP_FILE")"
elif [[ "$SETUP_STDIN" -eq 1 ]]; then
  setup_content="$(cat)"
else
  printf '请粘贴一次性设置码（输入内容不会显示）：' >&2
  IFS= read -r -s setup_content
  printf '\n' >&2
fi
# Keep one in-memory snapshot: confirmation and redemption use identical input.
preview="$(printf '%s' "$setup_content" | "$INSTALLED_BINARY" --setup-stdin --setup-preview)"
printf '请核对登记目标（以下不包含设置密钥）：\n%s\n' "$preview"
if [[ "$CONFIRM_ENROLLMENT" -ne 1 ]]; then
  if [[ "$SETUP_STDIN" -eq 1 ]]; then
    echo "标准输入设置需要 --confirm-enrollment；请先核对管理员提供的登记目标。" >&2
    exit 1
  fi
  printf '确认连接此登记地址并保存本机凭据？输入 yes 继续：' >&2
  IFS= read -r confirmation
  [[ "$confirmation" == 'yes' ]] || { echo "已取消，未进行登记。" >&2; exit 1; }
fi
printf '%s' "$setup_content" | "$INSTALLED_BINARY" --setup-stdin --setup-enroll \
  --setup-state "$INSTALL_DIR/setup-state.json" --setup-output "$CONNECTION_CONFIG"
unset setup_content
[[ -s "$CONNECTION_CONFIG" ]] || { echo "登记程序未生成连接配置。" >&2; exit 1; }

cp "$SOURCE_SKILL/SKILL.md" "$STANDARD_SKILL_DIR/SKILL.md"
cp "$SOURCE_SKILL/SKILL.md" "$COMPAT_SKILL_DIR/SKILL.md"
if [[ -d "$SOURCE_SKILL/agents" ]]; then
  mkdir -p "$STANDARD_SKILL_DIR/agents" "$COMPAT_SKILL_DIR/agents"
  cp "$SOURCE_SKILL/agents/openai.yaml" "$STANDARD_SKILL_DIR/agents/openai.yaml"
  cp "$SOURCE_SKILL/agents/openai.yaml" "$COMPAT_SKILL_DIR/agents/openai.yaml"
fi

# The helper parses TOML and preserves unrelated settings, quoted keys and
# multiline strings. Never edit Codex TOML with line-oriented substitutions.
"$INSTALLED_BINARY" --configure-codex "$CONFIG_PATH" \
  --mcp-command "$INSTALLED_BINARY" --mcp-config "$CONNECTION_CONFIG" --mcp-mode "$COMMAND_MODE"

find "$INSTALL_DIR" -maxdepth 1 -type f -name 'remoteops-controller-mcp-*' ! -name "$(basename "$INSTALLED_BINARY")" -delete

echo
echo "RemoteOps MCP $MCP_PACKAGE_VERSION 已安装。"
echo "程序：$INSTALLED_BINARY"
echo "配置：$CONFIG_PATH"
echo "Relay 配置：$CONNECTION_CONFIG"
echo "独立 Controller 凭据：已保存到 macOS Keychain；登记与 Relay 身份自检通过。"
echo "请完全退出并重新打开 Codex，然后输入 /mcp 检查 remoteops。"
