#!/usr/bin/env bash

set -euo pipefail
IFS=$'\n\t'

usage() {
    cat <<'EOF'
Usage:
  RELEASE_TAG=v1.4.9-rebasefix-20260601 \
  S3_BUCKET=your-bucket \
  PUBLIC_BASE_URL=https://downloads.example.com/git-ai/releases \
  scripts/release-s3-local.sh

Required environment variables:
  RELEASE_TAG       Internal release tag to publish under.
  S3_BUCKET         S3 bucket name.
  PUBLIC_BASE_URL   Public HTTPS base URL before the release tag.

Optional environment variables:
  S3_PREFIX         S3 prefix before the release tag. Default: git-ai/releases
  CHANNEL           Mutable channel name to update, e.g. latest or rebasefix.
  UPLOAD_TOOL       Upload backend: aws or s3cmd. Default: aws
  AWS_REGION        AWS region passed to aws s3 sync.
  AWS_PROFILE       AWS profile used by the AWS CLI.
  S3CMD_CONFIG      s3cmd config path. Default: ~/.s3cfg
  DRY_RUN           If 1, build package but do not upload. Default: 0
  RELEASE_DIR       Output directory. Default: release/s3-$RELEASE_TAG
  PACKAGE_DIR       Directory containing prebuilt git-ai-* binaries to publish.
  REQUIRE_ALL_PLATFORMS
                    If 1 with PACKAGE_DIR, require all official platform binaries. Default: 0
  SKIP_BUILD        If 1, skip cargo build and package an existing target/release/git-ai.
EOF
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
    usage
    exit 0
fi

require_env() {
    local name="$1"
    if [[ -z "${!name:-}" ]]; then
        echo "error: $name is required" >&2
        usage >&2
        exit 1
    fi
}

require_cmd() {
    local name="$1"
    if ! command -v "$name" >/dev/null 2>&1; then
        echo "error: required command not found: $name" >&2
        exit 1
    fi
}

detect_binary_name() {
    local os arch
    os="$(uname -s | tr '[:upper:]' '[:lower:]')"
    arch="$(uname -m)"

    case "$os" in
        darwin) os="macos" ;;
        linux) os="linux" ;;
        msys*|mingw*|cygwin*) os="windows" ;;
        *) echo "error: unsupported operating system: $os" >&2; exit 1 ;;
    esac

    case "$arch" in
        x86_64|amd64) arch="x64" ;;
        aarch64|arm64) arch="arm64" ;;
        *) echo "error: unsupported architecture: $arch" >&2; exit 1 ;;
    esac

    if [[ "$os" == "windows" ]]; then
        echo "git-ai-${os}-${arch}.exe"
    else
        echo "git-ai-${os}-${arch}"
    fi
}

sha256_file() {
    local file="$1"
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$file"
    else
        shasum -a 256 "$file"
    fi
}

required_platform_binaries=(
    git-ai-linux-x64
    git-ai-linux-arm64
    git-ai-macos-x64
    git-ai-macos-arm64
    git-ai-windows-x64.exe
    git-ai-windows-arm64.exe
)

write_binary_checksums() {
    local output_dir="$1"
    (
        cd "$output_dir"
        shopt -s nullglob
        binaries=(git-ai-*)
        shopt -u nullglob

        if [[ "${#binaries[@]}" -eq 0 ]]; then
            echo "error: no git-ai-* binaries found in $output_dir" >&2
            exit 1
        fi

        : > SHA256SUMS
        for binary in "${binaries[@]}"; do
            [[ "$binary" == "git-ai-"* ]] || continue
            sha256_file "$binary" >> SHA256SUMS
        done
    )
}

copy_prebuilt_binaries() {
    local input_dir="$1"
    local output_dir="$2"

    if [[ ! -d "$input_dir" ]]; then
        echo "error: PACKAGE_DIR not found: $input_dir" >&2
        exit 1
    fi

    if [[ "${REQUIRE_ALL_PLATFORMS:-0}" == "1" ]]; then
        for binary in "${required_platform_binaries[@]}"; do
            if [[ ! -f "$input_dir/$binary" ]]; then
                echo "error: required platform binary missing: $input_dir/$binary" >&2
                exit 1
            fi
        done
    fi

    shopt -s nullglob
    local binaries=("$input_dir"/git-ai-*)
    shopt -u nullglob

    if [[ "${#binaries[@]}" -eq 0 ]]; then
        echo "error: no git-ai-* binaries found in PACKAGE_DIR: $input_dir" >&2
        exit 1
    fi

    for binary in "${binaries[@]}"; do
        cp "$binary" "$output_dir/$(basename "$binary")"
        chmod +x "$output_dir/$(basename "$binary")" 2>/dev/null || true
    done
}

generate_install_scripts() {
    local output_dir="$1"
    local download_base_url="$2"
    local embedded_checksums="$3"

    awk -v version="$RELEASE_TAG" -v base_url="$download_base_url" -v checksums="$embedded_checksums" '
        /^PINNED_VERSION="__VERSION_PLACEHOLDER__"/ { sub(/__VERSION_PLACEHOLDER__/, version) }
        /^BASE_URL="__BASE_URL_PLACEHOLDER__"/ { sub(/__BASE_URL_PLACEHOLDER__/, base_url) }
        /^EMBEDDED_CHECKSUMS="__CHECKSUMS_PLACEHOLDER__"/ { sub(/__CHECKSUMS_PLACEHOLDER__/, checksums) }
        { print }
    ' install.sh > "$output_dir/install.sh"
    chmod +x "$output_dir/install.sh"

    awk -v version="$RELEASE_TAG" -v base_url="$download_base_url" -v checksums="$embedded_checksums" '
        /^[$]PinnedVersion = .__VERSION_PLACEHOLDER__/ { sub(/__VERSION_PLACEHOLDER__/, version) }
        /^[$]BaseUrl = .__BASE_URL_PLACEHOLDER__/ { sub(/__BASE_URL_PLACEHOLDER__/, base_url) }
        /^[$]EmbeddedChecksums = .__CHECKSUMS_PLACEHOLDER__/ { sub(/__CHECKSUMS_PLACEHOLDER__/, checksums) }
        { print }
    ' install.ps1 > "$output_dir/install.ps1"
}

append_install_script_checksums() {
    local output_dir="$1"
    (
        cd "$output_dir"
        sha256_file install.sh >> SHA256SUMS
        sha256_file install.ps1 >> SHA256SUMS
    )
}

upload_dir() {
    local src_dir="$1"
    local dest="$2"

    case "${UPLOAD_TOOL:-aws}" in
        aws)
            aws_args=()
            if [[ -n "${AWS_PROFILE:-}" ]]; then
                aws_args+=(--profile "$AWS_PROFILE")
            fi
            if [[ -n "${AWS_REGION:-}" ]]; then
                aws_args+=(--region "$AWS_REGION")
            fi
            aws "${aws_args[@]}" s3 sync "$src_dir/" "$dest" --delete
            ;;
        s3cmd)
            s3cmd_config="${S3CMD_CONFIG:-$HOME/.s3cfg}"
            if [[ ! -f "$s3cmd_config" ]]; then
                echo "error: s3cmd config not found: $s3cmd_config" >&2
                exit 1
            fi
            if command -v s3cmd >/dev/null 2>&1; then
                s3cmd -c "$s3cmd_config" put "$src_dir"/* "$dest"
            else
                uv tool run s3cmd -c "$s3cmd_config" put "$src_dir"/* "$dest"
            fi
            ;;
    esac
}

require_env RELEASE_TAG
require_env S3_BUCKET
require_env PUBLIC_BASE_URL

if [[ -z "${PACKAGE_DIR:-}" ]] && ! command -v cargo >/dev/null 2>&1 && [[ -f "$HOME/.cargo/env" ]]; then
    # shellcheck disable=SC1091
    source "$HOME/.cargo/env"
fi

if [[ -z "${PACKAGE_DIR:-}" ]]; then
    require_cmd cargo
fi
require_cmd awk
require_cmd sed

if [[ "${DRY_RUN:-0}" != "1" ]]; then
    case "${UPLOAD_TOOL:-aws}" in
        aws)
            require_cmd aws
            ;;
        s3cmd)
            if ! command -v s3cmd >/dev/null 2>&1 && ! command -v uv >/dev/null 2>&1; then
                echo "error: UPLOAD_TOOL=s3cmd requires either s3cmd or uv" >&2
                exit 1
            fi
            ;;
        *)
            echo "error: unsupported UPLOAD_TOOL: ${UPLOAD_TOOL:-}" >&2
            echo "supported values: aws, s3cmd" >&2
            exit 1
            ;;
    esac
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

s3_prefix="${S3_PREFIX:-git-ai/releases}"
s3_prefix="${s3_prefix#/}"
s3_prefix="${s3_prefix%/}"
release_dir="${RELEASE_DIR:-release/s3-$RELEASE_TAG}"
base_url="${PUBLIC_BASE_URL%/}/${RELEASE_TAG}"
binary_name="$(detect_binary_name)"

rm -rf "$release_dir"
mkdir -p "$release_dir"

if [[ -n "${PACKAGE_DIR:-}" ]]; then
    copy_prebuilt_binaries "$PACKAGE_DIR" "$release_dir"
elif [[ "${SKIP_BUILD:-0}" != "1" ]]; then
    cargo build --release --bin git-ai

    source_binary="target/release/git-ai"
    if [[ "$binary_name" == *.exe ]]; then
        source_binary="target/release/git-ai.exe"
    fi

    if [[ ! -f "$source_binary" ]]; then
        echo "error: built binary not found: $source_binary" >&2
        exit 1
    fi

    cp "$source_binary" "$release_dir/$binary_name"
    chmod +x "$release_dir/$binary_name" 2>/dev/null || true
    strip "$release_dir/$binary_name" 2>/dev/null || true
else
    source_binary="target/release/git-ai"
    if [[ "$binary_name" == *.exe ]]; then
        source_binary="target/release/git-ai.exe"
    fi

    if [[ ! -f "$source_binary" ]]; then
        echo "error: built binary not found: $source_binary" >&2
        exit 1
    fi

    cp "$source_binary" "$release_dir/$binary_name"
    chmod +x "$release_dir/$binary_name" 2>/dev/null || true
    strip "$release_dir/$binary_name" 2>/dev/null || true
fi

write_binary_checksums "$release_dir"

checksums="$(tr '\n' '|' < "$release_dir/SHA256SUMS" | sed 's/|$//')"
generate_install_scripts "$release_dir" "$base_url" "$checksums"
append_install_script_checksums "$release_dir"

channel_dir=""
channel_url=""
if [[ -n "${CHANNEL:-}" ]]; then
    channel_dir="$(dirname "$release_dir")/channel-${CHANNEL}"
    channel_url="${PUBLIC_BASE_URL%/}/channels/${CHANNEL}"
    rm -rf "$channel_dir"
    mkdir -p "$channel_dir"
    shopt -s nullglob
    for binary in "$release_dir"/git-ai-*; do
        cp "$binary" "$channel_dir/$(basename "$binary")"
    done
    shopt -u nullglob
    write_binary_checksums "$channel_dir"
    channel_checksums="$(tr '\n' '|' < "$channel_dir/SHA256SUMS" | sed 's/|$//')"
    generate_install_scripts "$channel_dir" "$channel_url" "$channel_checksums"
    append_install_script_checksums "$channel_dir"
fi

echo "Created local S3 release package:"
echo "  $release_dir"
echo
ls -la "$release_dir"
echo

if [[ "${DRY_RUN:-0}" == "1" ]]; then
    echo "DRY_RUN=1, skipping upload."
    echo "Install URL after upload would be:"
    echo "  ${base_url}/install.sh"
    if [[ -n "$channel_url" ]]; then
        echo "Channel install URL after upload would be:"
        echo "  ${channel_url}/install.sh"
    fi
    exit 0
fi

dest="s3://${S3_BUCKET}/${s3_prefix}/${RELEASE_TAG}/"
upload_dir "$release_dir" "$dest"

channel_dest=""
if [[ -n "$channel_dir" ]]; then
    channel_dest="s3://${S3_BUCKET}/${s3_prefix}/channels/${CHANNEL}/"
    upload_dir "$channel_dir" "$channel_dest"
fi

echo
echo "Uploaded release to:"
echo "  $dest"
if [[ -n "$channel_dest" ]]; then
    echo "Uploaded channel to:"
    echo "  $channel_dest"
fi
echo
echo "Install with:"
echo "  curl -fsSL ${base_url}/install.sh | bash"
if [[ -n "$channel_url" ]]; then
    echo
    echo "Channel install with:"
    echo "  curl -fsSL ${channel_url}/install.sh | bash"
fi
echo
echo "Windows install with:"
echo "  irm ${base_url}/install.ps1 | iex"
if [[ -n "$channel_url" ]]; then
    echo
    echo "Windows channel install with:"
    echo "  irm ${channel_url}/install.ps1 | iex"
fi
