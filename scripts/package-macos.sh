#!/usr/bin/env bash
# Builds Stick-itt.app as a universal binary (Apple Silicon + Intel), ad-hoc signs it, and
# packages it into dist/:
#   Stick-itt-<version>-macos.dmg   drag-to-Applications disk image, for downloading by hand
#   Stick-itt-<version>-macos.zip   the zipped app, for the Homebrew cask
# Needs Xcode command line tools and both Rust targets:
#   rustup target add aarch64-apple-darwin x86_64-apple-darwin
# Usage: scripts/package-macos.sh [version]   (default: the version in Cargo.toml)
set -euo pipefail
cd "$(dirname "$0")/.."

version="${1:-$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)}"
icon="assets/icon.png" # 1024x1024 master (scripts/make-icons.py)
[ -f "$icon" ] || { echo "error: $icon is missing" >&2; exit 1; }

for target in aarch64-apple-darwin x86_64-apple-darwin; do
  cargo build --release --locked --target "$target"
done

work="target/package"
app="$work/Stick-itt.app"
rm -rf "$work"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources" dist

target_dir="${CARGO_TARGET_DIR:-target}"
lipo -create -output "$app/Contents/MacOS/stickit" \
  "$target_dir/aarch64-apple-darwin/release/stickit" \
  "$target_dir/x86_64-apple-darwin/release/stickit"
sed "s/@VERSION@/$version/g" packaging/macos/Info.plist > "$app/Contents/Info.plist"

iconset="$work/AppIcon.iconset"
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
  sips -z "$size" "$size" "$icon" --out "$iconset/icon_${size}x${size}.png" > /dev/null
  sips -z $((size * 2)) $((size * 2)) "$icon" --out "$iconset/icon_${size}x${size}@2x.png" > /dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/AppIcon.icns"

# Ad-hoc signature: required to run on Apple Silicon. Not notarized (no Developer ID).
codesign --force --deep --sign - "$app"
codesign --verify --deep --strict "$app"

zip="dist/Stick-itt-$version-macos.zip"
rm -f "$zip"
ditto -c -k --keepParent "$app" "$zip"
echo "$zip"

# The disk image: the app next to a link to /Applications.
staging="$work/dmg"
mkdir -p "$staging"
ditto "$app" "$staging/Stick-itt.app"
ln -s /Applications "$staging/Applications"
dmg="dist/Stick-itt-$version-macos.dmg"
rm -f "$dmg"
hdiutil create -volname "Stick-itt" -srcfolder "$staging" -fs HFS+ -format UDZO -ov "$dmg" > /dev/null
echo "$dmg"
