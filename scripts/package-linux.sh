#!/usr/bin/env bash
# Build locally: no downloads of packaging tools and no remote build service.
set -euo pipefail
cd "$(dirname "$0")/.."
version=${1:-$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -1)}
case "$version" in ''|*[!a-zA-Z0-9.+-]*) echo 'Invalid package version' >&2; exit 1;; esac
arch=$(uname -m)
cargo build --release --locked
bundle="Stick-itt-$version-linux-$arch"
stage=$(mktemp -d)
trap 'rm -rf -- "$stage"' EXIT
mkdir -p "$stage/$bundle" dist
install -m755 "${CARGO_TARGET_DIR:-target}/release/stickit" "$stage/$bundle/stickit"
install -m755 packaging/linux/install.py "$stage/$bundle/install.py"
install -m644 packaging/linux/dev.stickitt.Stickitt.desktop assets/icon.png assets/icon-64.png LICENSE README.md "$stage/$bundle/"
tar -C "$stage" -czf "dist/$bundle.tar.gz" "$bundle"
(cd dist && sha256sum "$bundle.tar.gz" > "$bundle.tar.gz.sha256")
printf '%s\n' "dist/$bundle.tar.gz"
