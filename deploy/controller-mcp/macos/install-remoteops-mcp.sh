#!/bin/bash
set +x
set -euo pipefail
umask 077

MCP_PACKAGE_VERSION="0.2.0-preview.13"
CREDENTIAL_PROMPT_VERSION="0.2.0-preview.5"
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
SETUP_FILE=""
SETUP_STDIN=0
SETUP_CODE=0
CONFIRM_ENROLLMENT=0
SETUP_MODE=0
setup_content=""
trap 'unset setup_content controller_token' EXIT

usage() {
  cat <<'EOF'
用法：
  ./install-remoteops-mcp.sh --setup-file <设置文件路径> [--confirm-enrollment]
  ./install-remoteops-mcp.sh --setup-code
  ./install-remoteops-mcp.sh --setup-stdin [--confirm-enrollment]
  ./install-remoteops-mcp.sh --relay relay.example.com:7443 --owner-id <UUID> [选项]

选项：
  --setup-file <路径>         导入管理员签发的一次性设置文件
  --setup-code                隐藏提示粘贴一次性设置码；不接受命令行密钥值
  --setup-stdin               从标准输入读取设置文件内容或设置码
  --confirm-enrollment        已确认设置文件中的登记地址，允许非交互登记
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
    --setup-file) SETUP_FILE="${2:?--setup-file 需要文件路径}"; SETUP_MODE=$((SETUP_MODE + 1)); shift 2 ;;
    --setup-code) SETUP_CODE=1; SETUP_MODE=$((SETUP_MODE + 1)); shift ;;
    --setup-stdin) SETUP_STDIN=1; SETUP_MODE=$((SETUP_MODE + 1)); shift ;;
    --confirm-enrollment) CONFIRM_ENROLLMENT=1; shift ;;
    --relay) RELAY_ADDRESS="${2:-}"; shift 2 ;;
    --owner-id) OWNER_ID="${2:-}"; shift 2 ;;
    --server-name) SERVER_NAME="${2:-}"; shift 2 ;;
    --ca-cert) CA_CERT="${2:-}"; shift 2 ;;
    --tls-fingerprint) TLS_FINGERPRINT="${2:-}"; shift 2 ;;
    --command-mode) COMMAND_MODE="${2:-}"; shift 2 ;;
    --codex-home) CODEX_HOME="${2:-}"; shift 2 ;;
    --skip-token-prompt) SKIP_TOKEN_PROMPT=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "未知参数；一次性设置码只能通过隐藏提示、文件或标准输入提供。" >&2; usage >&2; exit 2 ;;
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
if [[ "$SETUP_MODE" -gt 1 ]]; then
  echo "--setup-file、--setup-code 和 --setup-stdin 只能选择一种。" >&2
  exit 1
fi
if [[ "$SETUP_MODE" -eq 1 ]]; then
  if [[ -n "$RELAY_ADDRESS$OWNER_ID$SERVER_NAME$CA_CERT$TLS_FINGERPRINT" || "$SKIP_TOKEN_PROMPT" -eq 1 ]]; then
    echo "一次性设置不能与手工 Relay、Owner、证书或 Token 参数混用。" >&2
    exit 1
  fi
elif [[ "$CONFIRM_ENROLLMENT" -eq 1 ]]; then
  echo "--confirm-enrollment 只能用于一次性设置。" >&2
  exit 1
fi
case "$COMMAND_MODE" in
  readonly|approval|agent-controlled|full-access) ;;
  *) echo "--command-mode 无效。" >&2; exit 1 ;;
esac
if [[ "$SETUP_MODE" -eq 0 ]]; then
  if [[ -z "$RELAY_ADDRESS" || ! "$RELAY_ADDRESS" =~ ^(\[[^]]+\]|[^:]+):[0-9]+$ ]]; then
    echo "--relay 必须使用 host:port 格式。" >&2
    exit 1
  fi
  if [[ ! "$OWNER_ID" =~ ^[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}$ || "$OWNER_ID" == "00000000-0000-0000-0000-000000000000" ]]; then
    echo "--owner-id 必须是非全零 UUID。" >&2
    exit 1
  fi
  if [[ -n "$CA_CERT" && -n "$TLS_FINGERPRINT" ]]; then
    echo "--ca-cert 和 --tls-fingerprint 只能选择一种。" >&2
    exit 1
  fi

fi

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

if [[ "$SETUP_MODE" -eq 0 ]]; then
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

fi

INSTALL_DIR="$CODEX_HOME/remoteops"
INSTALLED_BINARY="$INSTALL_DIR/remoteops-controller-mcp-$MCP_PACKAGE_VERSION"
INSTALLED_CREDENTIAL_PROMPT="$INSTALL_DIR/remoteops-credential-prompt"
LAUNCHER="$INSTALL_DIR/launch-remoteops-controller-mcp.sh"
CONNECTION_CONFIG="$INSTALL_DIR/controller-config.json"
CONFIG_PATH="$CODEX_HOME/config.toml"
STANDARD_SKILL_DIR="$HOME/.agents/skills/remoteops"
COMPAT_SKILL_DIR="$CODEX_HOME/skills/remoteops"
mkdir -p "$INSTALL_DIR" "$STANDARD_SKILL_DIR" "$COMPAT_SKILL_DIR"
install -m 755 "$SOURCE_BINARY" "$INSTALLED_BINARY"
install -m 755 "$SOURCE_CREDENTIAL_PROMPT" "$INSTALLED_CREDENTIAL_PROMPT"

MCP_COMMAND="$INSTALLED_BINARY"
if [[ "$SETUP_MODE" -eq 1 ]]; then
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
else
  INSTALLED_CA=""
  if [[ -n "$CA_CERT" ]]; then
    if [[ ! -f "$CA_CERT" ]]; then
      echo "找不到 CA 文件：$CA_CERT" >&2
      exit 1
    fi
    INSTALLED_CA="$INSTALL_DIR/relay-ca.pem"
    install -m 600 "$CA_CERT" "$INSTALLED_CA"
  fi

  # macOS 13's plutil cannot mutate a JSON file directly.  Build a tiny XML
  # property list, apply mutations, then convert it to the JSON config expected
  # by the Rust MCP.
  cat > "$CONNECTION_CONFIG" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict/></plist>
PLIST
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
  plutil -convert json -o "$CONNECTION_CONFIG.json" "$CONNECTION_CONFIG"
  mv "$CONNECTION_CONFIG.json" "$CONNECTION_CONFIG"
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

  MCP_COMMAND="$LAUNCHER"
fi

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
  --mcp-command "$MCP_COMMAND" --mcp-config "$CONNECTION_CONFIG" --mcp-mode "$COMMAND_MODE"

find "$INSTALL_DIR" -maxdepth 1 -type f -name 'remoteops-controller-mcp-*' ! -name "$(basename "$INSTALLED_BINARY")" -delete

echo
echo "RemoteOps MCP $MCP_PACKAGE_VERSION 已安装。"
echo "程序：$INSTALLED_BINARY"
echo "配置：$CONFIG_PATH"
echo "Relay 配置：$CONNECTION_CONFIG"
if [[ "$SETUP_MODE" -eq 1 ]]; then
  echo "独立 Controller 凭据：已保存到 macOS Keychain；登记与 Relay 身份自检通过。"
else
  echo "Token：已保存到当前用户的 macOS Keychain（未写入配置文件）。"
fi
echo "请完全退出并重新打开 Codex，然后输入 /mcp 检查 remoteops。"
