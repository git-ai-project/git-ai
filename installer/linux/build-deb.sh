#!/bin/bash
set -euo pipefail

# Build a .deb package for git-ai using only dpkg-deb (no third-party tools).
#
# Usage:
#   ./build-deb.sh --binary <path-to-git-ai> [--arch <amd64|arm64>] [--version <ver>] [--output <path>]

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BINARY_PATH=""
ARCHITECTURE="amd64"
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
    OUTPUT_PATH="$SCRIPT_DIR/build/git-ai_${VERSION}_${ARCHITECTURE}.deb"
fi

echo "Building deb: version=$VERSION arch=$ARCHITECTURE"
echo "  Binary: $BINARY_PATH"
echo "  Output: $OUTPUT_PATH"

# --- Setup build directory ---
BUILD_DIR="$SCRIPT_DIR/build/deb-${ARCHITECTURE}"
rm -rf "$BUILD_DIR"
mkdir -p "$BUILD_DIR/usr/lib/git-ai/bin"
mkdir -p "$BUILD_DIR/DEBIAN"

# --- Stage binaries ---
cp "$BINARY_PATH" "$BUILD_DIR/usr/lib/git-ai/bin/git-ai"
cp "$BINARY_PATH" "$BUILD_DIR/usr/lib/git-ai/bin/git"
chmod 755 "$BUILD_DIR/usr/lib/git-ai/bin/git-ai"
chmod 755 "$BUILD_DIR/usr/lib/git-ai/bin/git"

# --- Compute installed size (in KB) ---
INSTALLED_SIZE=$(du -sk "$BUILD_DIR/usr" | awk '{print $1}')

# --- Write control file ---
cat > "$BUILD_DIR/DEBIAN/control" << CTRL
Package: git-ai
Version: ${VERSION}
Architecture: ${ARCHITECTURE}
Maintainer: git-ai-project <dev@git-ai-project.com>
Installed-Size: ${INSTALLED_SIZE}
Depends: git
Section: devel
Priority: optional
Homepage: https://github.com/git-ai-project/git-ai
Description: AI-powered git attribution and authorship tracking
 git-ai transparently proxies git commands while tracking AI vs human
 authorship at the line level. It stores attribution data as git notes
 and supports checkpointing from IDE extensions and AI coding agents.
CTRL

# --- Write postinst script ---
cat > "$BUILD_DIR/DEBIAN/postinst" << 'POSTINST'
#!/bin/bash
set -e

INSTALL_DIR="/usr/lib/git-ai/bin"
PROFILE_FILE="/etc/profile.d/git-ai.sh"

# Create profile.d script to prepend our bin to PATH
cat > "$PROFILE_FILE" << EOF
# Added by git-ai package
export PATH="${INSTALL_DIR}:\$PATH"
EOF

chmod 644 "$PROFILE_FILE"
POSTINST
chmod 755 "$BUILD_DIR/DEBIAN/postinst"

# --- Write prerm script ---
cat > "$BUILD_DIR/DEBIAN/prerm" << 'PRERM'
#!/bin/bash
set -e

rm -f /etc/profile.d/git-ai.sh
PRERM
chmod 755 "$BUILD_DIR/DEBIAN/prerm"

# --- Build the package ---
mkdir -p "$(dirname "$OUTPUT_PATH")"
dpkg-deb --build --root-owner-group "$BUILD_DIR" "$OUTPUT_PATH"

# --- Done ---
DEB_SIZE=$(stat -c%s "$OUTPUT_PATH" 2>/dev/null || stat -f%z "$OUTPUT_PATH" 2>/dev/null)
echo ""
echo "Package built successfully!"
echo "  Path: $OUTPUT_PATH"
echo "  Size: $DEB_SIZE bytes"
echo "  Install: sudo dpkg -i \"$OUTPUT_PATH\""
