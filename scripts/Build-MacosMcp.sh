#!/bin/bash
set -euo pipefail

WORKSPACE_ROOT="$(cd "$(dirname "$0")/.." && pwd -P)"
TARGET="aarch64-apple-darwin"
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-13.0}"
VERSION="$(cargo metadata --manifest-path "$WORKSPACE_ROOT/Cargo.toml" --format-version 1 --no-deps --locked | python3 -c 'import json,sys; data=json.load(sys.stdin); print(next(p["version"] for p in data["packages"] if p["name"]=="remoteops-controller-mcp"))')"
OUTPUT_ROOT="${1:-$WORKSPACE_ROOT/artifacts/release/$VERSION/mcp}"
PACKAGE_NAME="RemoteOps-MCP-macOS-arm64-$VERSION"
PACKAGE_DIR="$OUTPUT_ROOT/$PACKAGE_NAME"
ARCHIVE_PATH="$OUTPUT_ROOT/$PACKAGE_NAME.tar.gz"
TEMPLATE_DIR="$WORKSPACE_ROOT/deploy/controller-mcp/macos"

export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=$WORKSPACE_ROOT=/remoteops --remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=/cargo --remap-path-prefix=$HOME=/user"
export CFLAGS="${CFLAGS:-} -ffile-prefix-map=$WORKSPACE_ROOT=/remoteops -fdebug-prefix-map=$WORKSPACE_ROOT=/remoteops -ffile-prefix-map=${CARGO_HOME:-$HOME/.cargo}=/cargo -fdebug-prefix-map=${CARGO_HOME:-$HOME/.cargo}=/cargo -ffile-prefix-map=$HOME=/user -fdebug-prefix-map=$HOME=/user"

rustup target add "$TARGET"
cargo build --manifest-path "$WORKSPACE_ROOT/Cargo.toml" --release --locked --target "$TARGET" -p remoteops-controller-mcp -p remoteops-credential-prompt

rm -rf "$PACKAGE_DIR"
rm -f "$ARCHIVE_PATH"
mkdir -p "$PACKAGE_DIR/skills/remoteops/agents"
install -m 755 "$WORKSPACE_ROOT/target/$TARGET/release/remoteops-controller-mcp" "$PACKAGE_DIR/remoteops-controller-mcp"
install -m 755 "$WORKSPACE_ROOT/target/$TARGET/release/remoteops-credential-prompt" "$PACKAGE_DIR/remoteops-credential-prompt"
install -m 755 "$TEMPLATE_DIR/install-remoteops-mcp.sh" "$PACKAGE_DIR/install-remoteops-mcp.sh"
install -m 755 "$TEMPLATE_DIR/test-remoteops-mcp.sh" "$PACKAGE_DIR/test-remoteops-mcp.sh"
install -m 755 "$TEMPLATE_DIR/uninstall-remoteops-mcp.sh" "$PACKAGE_DIR/uninstall-remoteops-mcp.sh"
install -m 644 "$TEMPLATE_DIR/README.md" "$PACKAGE_DIR/README.md"
install -m 644 "$TEMPLATE_DIR/skills/remoteops/SKILL.md" "$PACKAGE_DIR/skills/remoteops/SKILL.md"
install -m 644 "$TEMPLATE_DIR/skills/remoteops/agents/openai.yaml" "$PACKAGE_DIR/skills/remoteops/agents/openai.yaml"
pwsh -NoProfile -File "$WORKSPACE_ROOT/scripts/New-ThirdPartyNotices.ps1" -OutputDirectory "$PACKAGE_DIR"

lipo -archs "$PACKAGE_DIR/remoteops-controller-mcp" | grep -qw arm64
lipo -archs "$PACKAGE_DIR/remoteops-credential-prompt" | grep -qw arm64
"$PACKAGE_DIR/remoteops-controller-mcp" --version | grep -Fq "$VERSION"
"$PACKAGE_DIR/remoteops-credential-prompt" --version | grep -Fq "$VERSION"
pwsh -NoProfile -File "$WORKSPACE_ROOT/scripts/Test-ReleaseArtifacts.ps1" -ArtifactRoot "$PACKAGE_DIR" -RequireLegalFiles
tar -C "$OUTPUT_ROOT" -czf "$ARCHIVE_PATH" "$PACKAGE_NAME"
shasum -a 256 "$ARCHIVE_PATH"
echo "macOS MCP 安装包：$ARCHIVE_PATH"
