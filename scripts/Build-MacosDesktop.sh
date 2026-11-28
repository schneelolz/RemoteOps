#!/bin/bash
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd -P)"
cd "$root"
[[ "$(uname -sm)" == "Darwin arm64" ]] || { echo '需要 Apple Silicon Mac' >&2; exit 1; }
project="$root/apps/RemoteOps.Desktop/RemoteOps.Desktop.csproj"
version="$(/usr/bin/xmllint --xpath 'string(/Project/PropertyGroup/Version)' "$project")"
build_version="$(/usr/bin/xmllint --xpath 'string(/Project/PropertyGroup/FileVersion)' "$project")"
output="${1:-$root/artifacts/release/$version/desktop}"
mkdir -p "$output"
output="$(cd "$output" && pwd -P)"
product_name="RemoteOps Desktop"
if [[ -n "${DesktopProductProfile:-}" ]]; then
    product_name="$(plutil -extract productName raw "$DesktopProductProfile")"
    [[ -n "$product_name" && "$product_name" != *[/:]* && "$product_name" != *$'\n'* ]] || { echo '产品名称无效' >&2; exit 1; }
fi
name="$product_name.app"
archive="RemoteOps-Desktop-macOS-arm64-$version.zip"
[[ ! -e "$output/$name" && ! -e "$output/$archive" ]] || { echo '请使用新的输出目录' >&2; exit 1; }
stage="$(mktemp -d "$output/.desktop.XXXXXX")"
trap 'rm -rf "$stage"' EXIT
app="$stage/$name"
bin="$app/Contents/MacOS"
resources="$app/Contents/Resources"
mkdir -p "$bin" "$resources"

target=aarch64-apple-darwin
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-13.0}"
cargo build --locked --release --target "$target" -p remoteops-human-controller
target_dir="$(cargo metadata --no-deps --locked --format-version 1 | python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')"
dotnet publish "$project" -c Release -r osx-arm64 --self-contained true -o "$bin"
install -m 755 "$target_dir/$target/release/libremoteops_human_controller.dylib" "$bin/"
xcrun clang++ -std=c++17 -fobjc-arc -dynamiclib -arch arm64 \
    -install_name @rpath/libremoteops_view_decoder.dylib apps/RemoteOps.Desktop/native/decoder.mm \
    -framework Foundation -framework VideoToolbox -framework CoreMedia -framework CoreVideo \
    -o "$bin/libremoteops_view_decoder.dylib"

iconset="$stage/RemoteOps.iconset"
mkdir "$iconset"
for size in 16 32 128; do
    for scale in 1 2; do
        suffix=""
        if [[ "$scale" == 2 ]]; then suffix="@2x"; fi
        sips -z "$((size * scale))" "$((size * scale))" assets/brand/remoteops-mark.png \
            --out "$iconset/icon_${size}x${size}${suffix}.png" >/dev/null
    done
done
cp assets/brand/remoteops-mark.png "$iconset/icon_256x256.png"
iconutil -c icns "$iconset" -o "$resources/RemoteOps.icns"
plist="$app/Contents/Info.plist"
plutil -create xml1 "$plist"
plutil -insert CFBundleIdentifier -string 'com.remoteops.desktop' "$plist"
plutil -insert CFBundleExecutable -string 'RemoteOps.Desktop' "$plist"
plutil -insert CFBundleDisplayName -string "$product_name" "$plist"
plutil -insert CFBundleName -string "$product_name" "$plist"
plutil -insert CFBundleIconFile -string 'RemoteOps.icns' "$plist"
plutil -insert CFBundlePackageType -string 'APPL' "$plist"
plutil -insert CFBundleShortVersionString -string "${version%%-*}" "$plist"
plutil -insert CFBundleVersion -string "${build_version#0.}" "$plist"
plutil -insert LSMinimumSystemVersion -string "$MACOSX_DEPLOYMENT_TARGET" "$plist"
plutil -insert NSHighResolutionCapable -bool YES "$plist"
cp LICENSE "$resources/"
codesign --force --deep --sign - "$app"
codesign --verify --deep --strict "$app"
ditto -c -k --sequesterRsrc --keepParent "$app" "$stage/$archive"
mv "$app" "$stage/$archive" "$output/"
shasum -a 256 "$output/$archive"
