#!/usr/bin/env bash
set -euo pipefail
[[ $(uname -s) == Linux && $(uname -m) == x86_64 ]] || { echo 'Build natively on x86_64 Linux (Ubuntu 24.04 baseline).' >&2; exit 1; }
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
version=$(cargo pkgid --locked -p remoteops-agent | sed 's/.*[@#]//')
release_root=${1:-"$root/artifacts/release/$version/linux-x64"}
mkdir -p "$release_root"
release_root=$(realpath "$release_root")
package=$(mktemp -d "$release_root/RemoteOps-Agent-linux-x64-$version.XXXXXX")
chmod 0755 "$package"
export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=$root=/remoteops --remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=/cargo --remap-path-prefix=$HOME=/user"
export CFLAGS="${CFLAGS:-} -ffile-prefix-map=$root=/remoteops -fdebug-prefix-map=$root=/remoteops"
cargo build --release --locked -p remoteops-agent -p remoteops-agent-service
install -m 0755 target/release/remoteops-agent target/release/remoteops-agent-service "$package/"
install -m 0755 deploy/agent-service/linux/*.sh "$package/"
install -m 0644 deploy/agent-service/linux/{remoteops-agent.service,agent-config.example.json,README.md} LICENSE "$package/"
python3 scripts/linux-agent-package.py "$package"
(cd "$package"; find . -type f ! -name SHA256SUMS -print0 | sort -z | xargs -0 sha256sum > SHA256SUMS; sha256sum -c SHA256SUMS)
archive="$release_root/RemoteOps-Agent-linux-x64-$version.tar.gz"
tar -C "$package" -czf "$archive" .
sha256sum "$archive" > "$archive.sha256"
echo "PACKAGE_DIR=$package"
echo "ARCHIVE=$archive"
