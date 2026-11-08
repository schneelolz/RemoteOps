#!/bin/bash
set -euo pipefail

WORKSPACE_ROOT="$(cd "$(dirname "$0")/.." && pwd -P)"
TARGET="aarch64-apple-darwin"
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-13.0}"
read -r VERSION CREDENTIAL_PROMPT_VERSION <<<"$(cargo metadata --manifest-path "$WORKSPACE_ROOT/Cargo.toml" --format-version 1 --no-deps --locked | python3 -c 'import json,sys; data=json.load(sys.stdin); versions={p["name"]:p["version"] for p in data["packages"]}; print(versions["remoteops-controller-mcp"], versions["remoteops-credential-prompt"])')"
OUTPUT_ROOT="${1:-$WORKSPACE_ROOT/artifacts/release/$VERSION/mcp}"
PACKAGE_NAME="RemoteOps-MCP-macOS-arm64-$VERSION"
PACKAGE_DIR="$OUTPUT_ROOT/$PACKAGE_NAME"
ARCHIVE_PATH="$OUTPUT_ROOT/$PACKAGE_NAME.tar.gz"
TEMPLATE_DIR="$WORKSPACE_ROOT/deploy/controller-mcp/macos"

export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=$WORKSPACE_ROOT=/remoteops --remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=/cargo --remap-path-prefix=$HOME=/user"
export CFLAGS="${CFLAGS:-} -ffile-prefix-map=$WORKSPACE_ROOT=/remoteops -fdebug-prefix-map=$WORKSPACE_ROOT=/remoteops -ffile-prefix-map=${CARGO_HOME:-$HOME/.cargo}=/cargo -fdebug-prefix-map=${CARGO_HOME:-$HOME/.cargo}=/cargo -ffile-prefix-map=$HOME=/user -fdebug-prefix-map=$HOME=/user"

CARGO_OUTPUT="$(cargo metadata --manifest-path "$WORKSPACE_ROOT/Cargo.toml" --no-deps --locked --format-version 1 | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')"
rustup target add "$TARGET"
cargo build --manifest-path "$WORKSPACE_ROOT/Cargo.toml" --release --locked --target "$TARGET" -p remoteops-controller-mcp -p remoteops-credential-prompt

[[ ! -e "$PACKAGE_DIR" && ! -e "$ARCHIVE_PATH" ]] || { echo "输出已存在，请使用新的输出目录。" >&2; exit 1; }
mkdir -p "$PACKAGE_DIR"
install -m 755 "$CARGO_OUTPUT/$TARGET/release/remoteops-controller-mcp" "$PACKAGE_DIR/remoteops-controller-mcp"
install -m 755 "$CARGO_OUTPUT/$TARGET/release/remoteops-credential-prompt" "$PACKAGE_DIR/remoteops-credential-prompt"
install -m 755 "$TEMPLATE_DIR/install-remoteops-mcp.sh" "$PACKAGE_DIR/install-remoteops-mcp.sh"
install -m 755 "$TEMPLATE_DIR/test-remoteops-mcp.sh" "$PACKAGE_DIR/test-remoteops-mcp.sh"
install -m 755 "$TEMPLATE_DIR/uninstall-remoteops-mcp.sh" "$PACKAGE_DIR/uninstall-remoteops-mcp.sh"
install -m 644 "$TEMPLATE_DIR/README.md" "$PACKAGE_DIR/README.md"
pwsh -NoProfile -File "$WORKSPACE_ROOT/scripts/New-ThirdPartyNotices.ps1" -OutputDirectory "$PACKAGE_DIR"

lipo -archs "$PACKAGE_DIR/remoteops-controller-mcp" | grep -qw arm64
lipo -archs "$PACKAGE_DIR/remoteops-credential-prompt" | grep -qw arm64
"$PACKAGE_DIR/remoteops-controller-mcp" --version | grep -Fq "$VERSION"
"$PACKAGE_DIR/remoteops-credential-prompt" --version | grep -Fq "$CREDENTIAL_PROMPT_VERSION"
pwsh -NoProfile -File "$WORKSPACE_ROOT/scripts/Test-ReleaseArtifacts.ps1" -ArtifactRoot "$PACKAGE_DIR" -RequireLegalFiles
python3 - "$PACKAGE_DIR" "$VERSION" "$CREDENTIAL_PROMPT_VERSION" "$WORKSPACE_ROOT/Cargo.lock" <<'PY'
import hashlib, json, sys
from pathlib import Path
root = Path(sys.argv[1])
files = [{"file": p.relative_to(root).as_posix(), "bytes": p.stat().st_size,
          "sha256": hashlib.sha256(p.read_bytes()).hexdigest()}
         for p in sorted(root.rglob("*")) if p.is_file()]
manifest = {"schema_version": 1, "component": "remoteops-mcp", "version": sys.argv[2],
            "credential_prompt_version": sys.argv[3], "protocol": 20, "build_profile": "release",
            "lockfile_sha256": hashlib.sha256(Path(sys.argv[4]).read_bytes()).hexdigest(),
            "contains_credentials": False, "files": files}
(root / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
lines = [hashlib.sha256(p.read_bytes()).hexdigest() + "  " + p.relative_to(root).as_posix()
         for p in sorted(root.rglob("*")) if p.is_file()]
(root / "SHA256SUMS.txt").write_text("\n".join(lines) + "\n", encoding="utf-8")
PY
tar -C "$OUTPUT_ROOT" -czf "$ARCHIVE_PATH" "$PACKAGE_NAME"
shasum -a 256 "$ARCHIVE_PATH"
echo "macOS MCP 安装包：$ARCHIVE_PATH"
