#!/bin/bash
set -euo pipefail

# Updates the Homebrew formula template with release-specific values.
#
# Usage:
#   ./update-formula.sh --version <ver> --repo <owner/repo> --checksums <SHA256SUMS-file> [--output <path>]

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
VERSION=""
REPO=""
CHECKSUMS_FILE=""
OUTPUT_PATH=""

while [[ $# -gt 0 ]]; do
    case $1 in
        --version)    VERSION="$2"; shift 2 ;;
        --repo)       REPO="$2"; shift 2 ;;
        --checksums)  CHECKSUMS_FILE="$2"; shift 2 ;;
        --output)     OUTPUT_PATH="$2"; shift 2 ;;
        *)            echo "Unknown argument: $1"; exit 1 ;;
    esac
done

if [ -z "$VERSION" ] || [ -z "$REPO" ] || [ -z "$CHECKSUMS_FILE" ]; then
    echo "Error: --version, --repo, and --checksums are required"
    exit 1
fi

if [ ! -f "$CHECKSUMS_FILE" ]; then
    echo "Error: Checksums file not found: $CHECKSUMS_FILE"
    exit 1
fi

if [ -z "$OUTPUT_PATH" ]; then
    OUTPUT_PATH="$SCRIPT_DIR/build/git-ai.rb"
fi

mkdir -p "$(dirname "$OUTPUT_PATH")"

# Extract checksums for each platform binary
get_sha() {
    local name="$1"
    grep "$name" "$CHECKSUMS_FILE" | awk '{print $1}'
}

SHA_MACOS_ARM64=$(get_sha "git-ai-macos-arm64")
SHA_MACOS_X64=$(get_sha "git-ai-macos-x64")
SHA_LINUX_ARM64=$(get_sha "git-ai-linux-arm64")
SHA_LINUX_X64=$(get_sha "git-ai-linux-x64")

# Strip leading 'v' from version if present
VERSION="${VERSION#v}"

# Substitute placeholders
sed \
    -e "s|__VERSION__|${VERSION}|g" \
    -e "s|__REPO__|${REPO}|g" \
    -e "s|__SHA256_MACOS_ARM64__|${SHA_MACOS_ARM64}|g" \
    -e "s|__SHA256_MACOS_X64__|${SHA_MACOS_X64}|g" \
    -e "s|__SHA256_LINUX_ARM64__|${SHA_LINUX_ARM64}|g" \
    -e "s|__SHA256_LINUX_X64__|${SHA_LINUX_X64}|g" \
    "$SCRIPT_DIR/git-ai.rb" > "$OUTPUT_PATH"

echo "Formula generated: $OUTPUT_PATH"
echo "  Version: $VERSION"
echo "  Repo: $REPO"
