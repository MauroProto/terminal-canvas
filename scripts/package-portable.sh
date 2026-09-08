#!/usr/bin/env bash
# Linux portable package, including the sibling executables used by the app.
set -euo pipefail
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"
source "$REPO_ROOT/scripts/rust-toolchain.sh"
tc_select_rust_toolchain "$REPO_ROOT"
TARGET="${TC_PORTABLE_TARGET:-$("$TC_RUSTC" -vV | awk '/^host:/ {print $2}')}"
case "$TARGET" in
  x86_64-unknown-linux-gnu) ARCH="x86_64" ;;
  aarch64-unknown-linux-gnu) ARCH="aarch64" ;;
  *) echo "Target Linux no soportado: $TARGET" >&2; exit 1 ;;
esac
VERSION="$(awk -F'"' '/^version[[:space:]]*=/ {print $2; exit}' Cargo.toml)"
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[A-Za-z0-9.-]+)?$ ]] || { echo 'Version invalida' >&2; exit 1; }
PACKAGE_NAME="TerminalCanvas-$VERSION-linux-$ARCH"
"$TC_CARGO" build --release --locked --features daemon --bins --target "$TARGET"
mkdir -p dist
STAGE="$(mktemp -d "$REPO_ROOT/dist/.stage-XXXXXXXX")"
trap 'rm -rf "$STAGE"' EXIT
mkdir -p "$STAGE/$PACKAGE_NAME"
for binary in mi-terminal mi-terminal-daemon tc-memory tc-memory-mcp; do
  test -x "target/$TARGET/release/$binary"
  cp "target/$TARGET/release/$binary" "$STAGE/$PACKAGE_NAME/$binary"
done
cp LICENSE "$STAGE/$PACKAGE_NAME/"
cp docs/PORTABLE.md "$STAGE/$PACKAGE_NAME/PORTABLE.md"
tar -czf "dist/$PACKAGE_NAME.tar.gz" -C "$STAGE" "$PACKAGE_NAME"
(cd dist && sha256sum "$PACKAGE_NAME.tar.gz" > "$PACKAGE_NAME.tar.gz.sha256")
echo "Paquete: dist/$PACKAGE_NAME.tar.gz"
