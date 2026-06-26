#!/bin/bash
set -euo pipefail

# Build an .rpm package for git-ai using only rpmbuild (no third-party tools).
#
# Usage:
#   ./build-rpm.sh --binary <path-to-git-ai> [--arch <x86_64|aarch64>] [--version <ver>] [--output <path>]

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BINARY_PATH=""
ARCHITECTURE="x86_64"
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

echo "Building rpm: version=$VERSION arch=$ARCHITECTURE"
echo "  Binary: $BINARY_PATH"

# --- Setup rpmbuild directory tree ---
BUILD_DIR="$SCRIPT_DIR/build/rpm-${ARCHITECTURE}"
rm -rf "$BUILD_DIR"
mkdir -p "$BUILD_DIR"/{BUILD,RPMS,SOURCES,SPECS,SRPMS}

# --- Stage binaries in SOURCES (rpmbuild wipes BUILDROOT, not SOURCES) ---
cp "$BINARY_PATH" "$BUILD_DIR/SOURCES/git-ai"
cp "$BINARY_PATH" "$BUILD_DIR/SOURCES/git"
chmod 755 "$BUILD_DIR/SOURCES/git-ai"
chmod 755 "$BUILD_DIR/SOURCES/git"

# --- Write spec file ---
cat > "$BUILD_DIR/SPECS/git-ai.spec" << SPEC
Name:           git-ai
Version:        ${VERSION}
Release:        1
Summary:        AI-powered git attribution and authorship tracking
License:        MIT
URL:            https://github.com/git-ai-project/git-ai
Requires:       git

%description
git-ai transparently proxies git commands while tracking AI vs human
authorship at the line level. It stores attribution data as git notes
and supports checkpointing from IDE extensions and AI coding agents.

%install
mkdir -p %{buildroot}/usr/lib/git-ai/bin
cp %{_sourcedir}/git-ai %{buildroot}/usr/lib/git-ai/bin/git-ai
cp %{_sourcedir}/git %{buildroot}/usr/lib/git-ai/bin/git

%files
%attr(755, root, root) /usr/lib/git-ai/bin/git-ai
%attr(755, root, root) /usr/lib/git-ai/bin/git

%post
INSTALL_DIR="/usr/lib/git-ai/bin"
PROFILE_FILE="/etc/profile.d/git-ai.sh"

cat > "\$PROFILE_FILE" << EOF
# Added by git-ai package
export PATH="\${INSTALL_DIR}:\\\$PATH"
EOF

chmod 644 "\$PROFILE_FILE"

%preun
rm -f /etc/profile.d/git-ai.sh
SPEC

# --- Build the RPM ---
rpmbuild \
    --define "_topdir $BUILD_DIR" \
    --define "_rpmdir $BUILD_DIR/RPMS" \
    --target "$ARCHITECTURE" \
    -bb "$BUILD_DIR/SPECS/git-ai.spec"

# --- Move output to expected location ---
RPM_FILE=$(find "$BUILD_DIR/RPMS" -name "*.rpm" | head -1)
if [ -z "$RPM_FILE" ]; then
    echo "Error: rpmbuild did not produce an RPM"
    exit 1
fi

if [ -z "$OUTPUT_PATH" ]; then
    OUTPUT_PATH="$SCRIPT_DIR/build/git-ai-${VERSION}-1.${ARCHITECTURE}.rpm"
fi

mkdir -p "$(dirname "$OUTPUT_PATH")"
mv "$RPM_FILE" "$OUTPUT_PATH"

# --- Done ---
RPM_SIZE=$(stat -c%s "$OUTPUT_PATH" 2>/dev/null || stat -f%z "$OUTPUT_PATH" 2>/dev/null)
echo ""
echo "Package built successfully!"
echo "  Path: $OUTPUT_PATH"
echo "  Size: $RPM_SIZE bytes"
echo "  Install: sudo rpm -i \"$OUTPUT_PATH\""
