#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat >&2 <<'USAGE'
usage: build-deb.sh --binary <path> --arch <amd64|arm64> --version <version> --output <path>
USAGE
}

BINARY=""
ARCH=""
VERSION=""
OUTPUT=""

while [ "$#" -gt 0 ]; do
  case "$1" in
    --binary) BINARY="${2:-}"; shift 2 ;;
    --arch) ARCH="${2:-}"; shift 2 ;;
    --version) VERSION="${2:-}"; shift 2 ;;
    --output) OUTPUT="${2:-}"; shift 2 ;;
    *) usage; exit 2 ;;
  esac
done

[ -n "$BINARY" ] && [ -n "$ARCH" ] && [ -n "$VERSION" ] && [ -n "$OUTPUT" ] || { usage; exit 2; }
[ -f "$BINARY" ] || { echo "binary not found: $BINARY" >&2; exit 1; }

case "$ARCH" in
  amd64|arm64) ;;
  *) echo "unsupported arch: $ARCH" >&2; exit 2 ;;
esac

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORK_DIR="$ROOT/target/package/deb-$ARCH"
PKG_ROOT="$WORK_DIR/root"
DEBIAN_DIR="$PKG_ROOT/DEBIAN"
OUTPUT_ABS="$(python3 -c 'import os,sys; print(os.path.abspath(sys.argv[1]))' "$OUTPUT")"

rm -rf "$WORK_DIR"
mkdir -p "$DEBIAN_DIR" "$PKG_ROOT/usr/bin" "$PKG_ROOT/usr/share/doc/git-ai" "$(dirname "$OUTPUT_ABS")"
install -m 0755 "$BINARY" "$PKG_ROOT/usr/bin/git-ai"
install -m 0644 "$ROOT/README.md" "$PKG_ROOT/usr/share/doc/git-ai/README.md"

sed \
  -e "s/@VERSION@/$VERSION/g" \
  -e "s/@ARCH@/$ARCH/g" \
  "$ROOT/packaging/debian/control" > "$DEBIAN_DIR/control"
install -m 0755 "$ROOT/packaging/debian/postinst" "$DEBIAN_DIR/postinst"
install -m 0755 "$ROOT/packaging/debian/prerm" "$DEBIAN_DIR/prerm"
find "$PKG_ROOT" -type d -exec chmod 0755 {} +

dpkg-deb --build --root-owner-group "$PKG_ROOT" "$OUTPUT_ABS"
echo "Built deb: $OUTPUT_ABS"
