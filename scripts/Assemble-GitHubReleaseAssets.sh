#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 3 ]]; then
  echo 'Usage: Assemble-GitHubReleaseAssets.sh ARTIFACT_ROOT VERSION ASSET_ROOT' >&2
  exit 1
fi
artifact_root="$(cd "$1" && pwd)"
version="$2"
if [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]]; then
  echo 'Invalid release version' >&2
  exit 1
fi
mkdir -p "$3"
asset_root="$(cd "$3" && pwd)"
if [[ -n "$(find "$asset_root" -mindepth 1 -print -quit)" ]]; then
  echo 'Release asset directory must be empty' >&2
  exit 1
fi

find_one() {
  local match
  match="$(find "$artifact_root" "$@")"
  if [[ -z "$match" || "$match" == *$'\n'* ]]; then
    echo "Expected exactly one build artifact: $*" >&2
    return 1
  fi
  printf '%s\n' "$match"
}

windows_dir="$(find_one -type d -name windows-x64)"
mcp_zip="$(find_one -type f -name 'RemoteOps-MCP-Windows-x64-*.zip')"
macos_mcp="$(find_one -type f -name 'RemoteOps-MCP-macOS-arm64-*.tar.gz')"
relay_bin="$(find_one -type f -path '*/linux-x64/remoteops-relay')"
linux_agent="$(find_one -type f -name "RemoteOps-Agent-linux-x64-$version.tar.gz")"

(cd "$windows_dir" && zip -q -9 -r "$asset_root/RemoteOps-Windows-x64-$version.zip" .)
cp "$mcp_zip" "$asset_root/RemoteOps-MCP-Windows-x64-$version.zip"
cp "$macos_mcp" "$asset_root/RemoteOps-MCP-macOS-arm64-$version.tar.gz"
# Actions artifact upload/download does not retain executable file permissions.
chmod 755 "$relay_bin"
tar -C "$(dirname "$relay_bin")" -czf "$asset_root/RemoteOps-Relay-Linux-x64-$version.tar.gz" \
  remoteops-relay LICENSE THIRD_PARTY_LICENSES.txt DEPENDENCIES.json
cp "$linux_agent" "$asset_root/"
(
  cd "$asset_root"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum RemoteOps-* > SHA256SUMS.txt
  else
    shasum -a 256 RemoteOps-* > SHA256SUMS.txt
  fi
)
