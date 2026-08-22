#!/bin/bash
set -euo pipefail

PACKAGE_VERSION="0.2.0-preview.1"
KEYCHAIN_SERVICE="RemoteOps Controller Token"
CURRENT_USER="$(id -un)"
CODEX_HOME="${CODEX_HOME:-$HOME/.codex}"
RELAY_ADDRESS=""
OWNER_ID=""
SERVER_NAME=""
CA_CERT=""
TLS_FINGERPRINT=""
COMMAND_MODE="agent-controlled"
SKIP_TOKEN_PROMPT=0

usage() {
  cat <<'EOF'
用法：
  ./install-remoteops-mcp.sh --relay relay.example.com:7443 --owner-id <UUID> [选项]

选项：
  --server-name <名称>        TLS 证书服务名；默认从 Relay 地址推导
  --ca-cert <路径>            私有 CA PEM
  --tls-fingerprint <SHA256>  已独立核对的 Relay 叶证书指纹
  --command-mode <模式>       readonly|approval|agent-controlled|full-access
  --codex-home <路径>         默认 ~/.codex
  --skip-token-prompt         使用 Keychain 中已有的 Token
  -h, --help                  显示帮助
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --relay) RELAY_ADDRESS="${2:-}"; shift 2 ;;
    --owner-id) OWNER_ID="${2:-}"; shift 2 ;;
    --server-name) SERVER_NAME="${2:-}"; shift 2 ;;
    --ca-cert) CA_CERT="${2:-}"; shift 2 ;;
    --tls-fingerprint) TLS_FINGERPRINT="${2:-}"; shift 2 ;;
    --command-mode) COMMAND_MODE="${2:-}"; shift 2 ;;
    --codex-home) CODEX_HOME="${2:-}"; shift 2 ;;
    --skip-token-prompt) SKIP_TOKEN_PROMPT=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "未知参数：$1" >&2; usage >&2; exit 2 ;;
  esac
done

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "此安装包仅支持 macOS。" >&2
  exit 1
fi
if [[ "$(uname -m)" != "arm64" ]]; then
  echo "此安装包仅支持 Apple Silicon（arm64）Mac。" >&2
  exit 1
fi
if [[ -z "$RELAY_ADDRESS" || ! "$RELAY_ADDRESS" =~ ^(\[[^]]+\]|[^:]+):[0-9]+$ ]]; then
  echo "--relay 必须使用 host:port 格式。" >&2
  exit 1
fi
if [[ ! "$OWNER_ID" =~ ^[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}$ || "$OWNER_ID" == "00000000-0000-0000-0000-000000000000" ]]; then
  echo "--owner-id 必须是非全零 UUID。" >&2
  exit 1
fi
case "$COMMAND_MODE" in
  readonly|approval|agent-controlled|full-access) ;;
  *) echo "--command-mode 无效。" >&2; exit 1 ;;
esac
if [[ -n "$CA_CERT" && -n "$TLS_FINGERPRINT" ]]; then
  echo "--ca-cert 和 --tls-fingerprint 只能选择一种。" >&2
  exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd -P)"
SOURCE_BINARY="$SCRIPT_DIR/remoteops-controller-mcp"
SOURCE_SKILL="$SCRIPT_DIR/skills/remoteops"
if [[ ! -x "$SOURCE_BINARY" ]]; then
  echo "安装包缺少可执行文件：$SOURCE_BINARY" >&2
  exit 1
fi
if [[ ! -f "$SOURCE_SKILL/SKILL.md" ]]; then
  echo "安装包缺少 RemoteOps skill。" >&2
  exit 1
fi
if ! "$SOURCE_BINARY" --version | grep -Fq "$PACKAGE_VERSION"; then
  echo "MCP 可执行文件版本与安装包不一致。" >&2
  exit 1
fi

if [[ -z "$SERVER_NAME" ]]; then
  if [[ "$RELAY_ADDRESS" =~ ^\[([^]]+)\]: ]]; then
    SERVER_NAME="${BASH_REMATCH[1]}"
  else
    SERVER_NAME="${RELAY_ADDRESS%:*}"
  fi
fi

if [[ -n "$TLS_FINGERPRINT" ]]; then
  compact="$(printf '%s' "$TLS_FINGERPRINT" | sed -E 's/^[Ss][Hh][Aa]256://; s/[:[:space:]-]//g')"
  if [[ ! "$compact" =~ ^[0-9A-Fa-f]{64}$ ]]; then
    echo "--tls-fingerprint 必须是 64 位 SHA-256 十六进制值。" >&2
    exit 1
  fi
  TLS_FINGERPRINT="$(printf '%s' "$compact" | tr '[:lower:]' '[:upper:]' | sed 's/../&:/g; s/:$//')"
fi

if [[ "$SKIP_TOKEN_PROMPT" -eq 0 ]]; then
  printf '请输入 AI Controller Token（输入内容不会显示）：' >&2
  IFS= read -r -s controller_token
  printf '\n' >&2
  if [[ ${#controller_token} -lt 32 ]]; then
    unset controller_token
    echo "AI Controller Token 至少需要 32 个字符。" >&2
    exit 1
  fi
  security add-generic-password -U -a "$CURRENT_USER" -s "$KEYCHAIN_SERVICE" -w "$controller_token" >/dev/null
  unset controller_token
elif ! security find-generic-password -a "$CURRENT_USER" -s "$KEYCHAIN_SERVICE" -w >/dev/null 2>&1; then
  echo "Keychain 中未找到 RemoteOps Controller Token。" >&2
  exit 1
fi

INSTALL_DIR="$CODEX_HOME/remoteops"
INSTALLED_BINARY="$INSTALL_DIR/remoteops-controller-mcp-$PACKAGE_VERSION"
LAUNCHER="$INSTALL_DIR/launch-remoteops-controller-mcp.sh"
CONNECTION_CONFIG="$INSTALL_DIR/controller-config.json"
CONFIG_PATH="$CODEX_HOME/config.toml"
STANDARD_SKILL_DIR="$HOME/.agents/skills/remoteops"
COMPAT_SKILL_DIR="$CODEX_HOME/skills/remoteops"
mkdir -p "$INSTALL_DIR" "$STANDARD_SKILL_DIR" "$COMPAT_SKILL_DIR"
install -m 755 "$SOURCE_BINARY" "$INSTALLED_BINARY"

INSTALLED_CA=""
if [[ -n "$CA_CERT" ]]; then
  if [[ ! -f "$CA_CERT" ]]; then
    echo "找不到 CA 文件：$CA_CERT" >&2
    exit 1
  fi
  INSTALLED_CA="$INSTALL_DIR/relay-ca.pem"
  install -m 600 "$CA_CERT" "$INSTALLED_CA"
fi

plutil -create json "$CONNECTION_CONFIG"
plutil -insert relay -string "$RELAY_ADDRESS" "$CONNECTION_CONFIG"
plutil -insert server_name -string "$SERVER_NAME" "$CONNECTION_CONFIG"
plutil -insert reconnect_seconds -integer 2 "$CONNECTION_CONFIG"
plutil -insert owner_id -string "$OWNER_ID" "$CONNECTION_CONFIG"
if [[ -n "$INSTALLED_CA" ]]; then
  plutil -insert ca_cert -string "$INSTALLED_CA" "$CONNECTION_CONFIG"
fi
if [[ -n "$TLS_FINGERPRINT" ]]; then
  plutil -insert tls_fingerprint -string "$TLS_FINGERPRINT" "$CONNECTION_CONFIG"
fi
chmod 600 "$CONNECTION_CONFIG"

cat > "$LAUNCHER" <<EOF
#!/bin/bash
set -euo pipefail
token="\$(security find-generic-password -a "$CURRENT_USER" -s "$KEYCHAIN_SERVICE" -w 2>/dev/null)" || {
  echo "RemoteOps MCP 无法从 macOS Keychain 读取 Controller Token。请重新运行安装器。" >&2
  exit 1
}
export REMOTEOPS_CONTROLLER_TOKEN="\$token"
unset token
exec "$INSTALLED_BINARY" "\$@"
EOF
chmod 700 "$LAUNCHER"

cp "$SOURCE_SKILL/SKILL.md" "$STANDARD_SKILL_DIR/SKILL.md"
cp "$SOURCE_SKILL/SKILL.md" "$COMPAT_SKILL_DIR/SKILL.md"
if [[ -d "$SOURCE_SKILL/agents" ]]; then
  mkdir -p "$STANDARD_SKILL_DIR/agents" "$COMPAT_SKILL_DIR/agents"
  cp "$SOURCE_SKILL/agents/openai.yaml" "$STANDARD_SKILL_DIR/agents/openai.yaml"
  cp "$SOURCE_SKILL/agents/openai.yaml" "$COMPAT_SKILL_DIR/agents/openai.yaml"
fi

toml_escape() {
  printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g'
}
mkdir -p "$CODEX_HOME"
if [[ -f "$CONFIG_PATH" ]]; then
  BACKUP_PATH="$CONFIG_PATH.remoteops-backup-$(date +%Y%m%d-%H%M%S)"
  cp "$CONFIG_PATH" "$BACKUP_PATH"
else
  touch "$CONFIG_PATH"
fi
TEMP_CONFIG="$(mktemp "${TMPDIR:-/tmp}/remoteops-config.XXXXXX")"
awk '
  BEGIN {
    print "approval_policy = { granular = { sandbox_approval = true, rules = true, mcp_elicitations = true, request_permissions = false, skill_approval = false } }"
  }
  /^\[/ {
    skip=($0 == "[approval_policy.granular]" || $0 ~ /^\[mcp_servers\.remoteops(\.|\])/)
    if (!skip) print
    next
  }
  skip { next }
  /^[[:space:]]*approval_policy[[:space:]]*=/ { next }
  { print }
' "$CONFIG_PATH" > "$TEMP_CONFIG"
while [[ -s "$TEMP_CONFIG" && -z "$(tail -n 1 "$TEMP_CONFIG" | tr -d '[:space:]')" ]]; do
  sed -i '' -e '$d' "$TEMP_CONFIG"
done
if [[ -s "$TEMP_CONFIG" ]]; then printf '\n' >> "$TEMP_CONFIG"; fi
cat >> "$TEMP_CONFIG" <<EOF
[mcp_servers.remoteops]
command = "$(toml_escape "$LAUNCHER")"
args = ["--config", "$(toml_escape "$CONNECTION_CONFIG")", "--command-mode", "$COMMAND_MODE"]
startup_timeout_sec = 15
tool_timeout_sec = 180
enabled = true
required = false
default_tools_approval_mode = "approve"

[mcp_servers.remoteops.tools.set_control_mode]
approval_mode = "prompt"
EOF
mv "$TEMP_CONFIG" "$CONFIG_PATH"
chmod 600 "$CONFIG_PATH"

find "$INSTALL_DIR" -maxdepth 1 -type f -name 'remoteops-controller-mcp-*' ! -name "$(basename "$INSTALLED_BINARY")" -delete

echo
echo "RemoteOps MCP $PACKAGE_VERSION 已安装。"
echo "程序：$INSTALLED_BINARY"
echo "配置：$CONFIG_PATH"
echo "Relay 配置：$CONNECTION_CONFIG"
echo "Token：已保存到当前用户的 macOS Keychain（未写入配置文件）。"
echo "请完全退出并重新打开 Codex，然后输入 /mcp 检查 remoteops。"
