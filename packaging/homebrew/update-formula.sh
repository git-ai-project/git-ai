#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat >&2 <<'USAGE'
usage: update-formula.sh --version <version> --checksums <SHA256SUMS> --output <Formula/git-ai.rb>
USAGE
}

VERSION=""
CHECKSUMS=""
OUTPUT=""

while [ "$#" -gt 0 ]; do
  case "$1" in
    --version) VERSION="${2:-}"; shift 2 ;;
    --checksums) CHECKSUMS="${2:-}"; shift 2 ;;
    --output) OUTPUT="${2:-}"; shift 2 ;;
    *) usage; exit 2 ;;
  esac
done

[ -n "$VERSION" ] && [ -n "$CHECKSUMS" ] && [ -n "$OUTPUT" ] || { usage; exit 2; }
[ -f "$CHECKSUMS" ] || { echo "checksums file not found: $CHECKSUMS" >&2; exit 1; }

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TEMPLATE="$ROOT/packaging/homebrew/git-ai.rb.template"

checksum_for() {
  local name="$1"
  awk -v wanted="$name" '$2 == wanted { print $1 }' "$CHECKSUMS"
}

SHA256_MACOS_ARM64="$(checksum_for git-ai-macos-arm64)"
SHA256_MACOS_X64="$(checksum_for git-ai-macos-x64)"
SHA256_LINUX_ARM64="$(checksum_for git-ai-linux-arm64)"
SHA256_LINUX_X64="$(checksum_for git-ai-linux-x64)"

for value_name in SHA256_MACOS_ARM64 SHA256_MACOS_X64 SHA256_LINUX_ARM64 SHA256_LINUX_X64; do
  if [ -z "${!value_name}" ]; then
    echo "missing checksum for $value_name" >&2
    exit 1
  fi
done

mkdir -p "$(dirname "$OUTPUT")"
sed \
  -e "s/@VERSION@/$VERSION/g" \
  -e "s/@SHA256_MACOS_ARM64@/$SHA256_MACOS_ARM64/g" \
  -e "s/@SHA256_MACOS_X64@/$SHA256_MACOS_X64/g" \
  -e "s/@SHA256_LINUX_ARM64@/$SHA256_LINUX_ARM64/g" \
  -e "s/@SHA256_LINUX_X64@/$SHA256_LINUX_X64/g" \
  "$TEMPLATE" > "$OUTPUT"

echo "Rendered Homebrew formula: $OUTPUT"
