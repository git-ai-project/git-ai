#!/bin/bash
set -euo pipefail

# Build a macOS .pkg installer for git-ai.
#
# Usage:
#   ./build-pkg.sh --binary <path-to-git-ai> [--arch <arm64|x64>] [--version <ver>] [--output <path>]

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BINARY_PATH=""
ARCHITECTURE="arm64"
VERSION=""
OUTPUT_PATH=""

while [[ $# -gt 0 ]]; do
    case $1 in
        --binary)   BINARY_PATH="$2"; shift 2 ;;
        --arch)     ARCHITECTURE="$2"; shift 2 ;;
        --version)  VERSION="$2"; shift 2 ;;
        --output)   OUTPUT_PATH="$2"; shift 2 ;;
        *)          echo "Unknown argument: $1"; exit 1 ;;
    esac
done

if [ -z "$BINARY_PATH" ]; then
    echo "Error: --binary is required"
    exit 1
fi

if [ ! -f "$BINARY_PATH" ]; then
    echo "Error: Binary not found: $BINARY_PATH"
    exit 1
fi

# Resolve version from Cargo.toml if not provided
if [ -z "$VERSION" ]; then
    CARGO_TOML="$SCRIPT_DIR/../../Cargo.toml"
    if [ -f "$CARGO_TOML" ]; then
        VERSION=$(grep '^version = ' "$CARGO_TOML" | cut -d'"' -f2)
    fi
    if [ -z "$VERSION" ]; then
        echo "Error: Could not determine version. Pass --version explicitly."
        exit 1
    fi
fi

if [ -z "$OUTPUT_PATH" ]; then
    OUTPUT_PATH="$SCRIPT_DIR/build/git-ai-macos-${ARCHITECTURE}.pkg"
fi

echo "Building pkg: version=$VERSION arch=$ARCHITECTURE"
echo "  Binary: $BINARY_PATH"
echo "  Output: $OUTPUT_PATH"

# --- Setup build directory ---
BUILD_DIR="$SCRIPT_DIR/build"
PAYLOAD_DIR="$BUILD_DIR/payload/Library/git-ai/bin"

rm -rf "$BUILD_DIR"
mkdir -p "$PAYLOAD_DIR"
mkdir -p "$(dirname "$OUTPUT_PATH")"

# --- Stage binaries ---
cp "$BINARY_PATH" "$PAYLOAD_DIR/git-ai"
cp "$BINARY_PATH" "$PAYLOAD_DIR/git"
chmod 755 "$PAYLOAD_DIR/git-ai"
chmod 755 "$PAYLOAD_DIR/git"

# --- Build the component package ---
IDENTIFIER="com.git-ai-project.git-ai"

pkgbuild \
    --root "$BUILD_DIR/payload" \
    --identifier "$IDENTIFIER" \
    --version "$VERSION" \
    --install-location "/" \
    --scripts "$SCRIPT_DIR/scripts" \
    "$OUTPUT_PATH"

# --- Done ---
PKG_SIZE=$(stat -f%z "$OUTPUT_PATH" 2>/dev/null || stat -c%s "$OUTPUT_PATH" 2>/dev/null)
echo ""
echo "Package built successfully!"
echo "  Path: $OUTPUT_PATH"
echo "  Size: $PKG_SIZE bytes"
echo "  Install: sudo installer -pkg \"$OUTPUT_PATH\" -target /"
