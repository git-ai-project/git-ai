#!/bin/sh
# Usage: curl -fsSL https://raw.githubusercontent.com/git-ai-inc/git-ai/main/scripts/install.sh | sh
#
# Installs the latest git-ai release for the current platform.
# Detects OS (Linux/macOS) and architecture (x86_64/aarch64).
# Downloads the correct release tarball from GitHub.
# Extracts to ~/.git-ai/bin/ and runs `git-ai install`.

set -eu

# --- Configuration ---
REPO="git-ai-inc/git-ai"
INSTALL_DIR="$HOME/.git-ai/bin"
GITHUB_BASE="https://github.com/${REPO}/releases"

# --- Colors ---
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
NC='\033[0m'

# --- Helpers ---
error() {
    printf "${RED}error:${NC} %s\n" "$1" >&2
    exit 1
}

warn() {
    printf "${YELLOW}warning:${NC} %s\n" "$1" >&2
}

success() {
    printf "${GREEN}%s${NC}\n" "$1"
}

need_cmd() {
    if ! command -v "$1" > /dev/null 2>&1; then
        error "need '$1' (command not found)"
    fi
}

# --- Detect platform ---
detect_platform() {
    OS="$(uname -s)"
    ARCH="$(uname -m)"

    case "$OS" in
        Linux)  OS_TARGET="unknown-linux-gnu" ;;
        Darwin) OS_TARGET="apple-darwin" ;;
        *)      error "unsupported OS: $OS" ;;
    esac

    case "$ARCH" in
        x86_64|amd64)       ARCH_TARGET="x86_64" ;;
        aarch64|arm64)      ARCH_TARGET="aarch64" ;;
        *)                  error "unsupported architecture: $ARCH" ;;
    esac

    TARGET="${ARCH_TARGET}-${OS_TARGET}"
}

# --- Determine version ---
get_version() {
    if [ -n "${GIT_AI_VERSION:-}" ]; then
        VERSION="$GIT_AI_VERSION"
    else
        VERSION="latest"
    fi
}

# --- Download ---
download_release() {
    TARBALL_NAME="git-ai-${TARGET}.tar.gz"

    if [ "$VERSION" = "latest" ]; then
        DOWNLOAD_URL="${GITHUB_BASE}/latest/download/${TARBALL_NAME}"
        CHECKSUM_URL="${GITHUB_BASE}/latest/download/SHA256SUMS"
    else
        DOWNLOAD_URL="${GITHUB_BASE}/download/${VERSION}/${TARBALL_NAME}"
        CHECKSUM_URL="${GITHUB_BASE}/download/${VERSION}/SHA256SUMS"
    fi

    TMP_DIR="$(mktemp -d)"
    trap 'rm -rf "$TMP_DIR"' EXIT

    printf "Downloading git-ai (%s)...\n" "$VERSION"

    if command -v curl > /dev/null 2>&1; then
        HTTP_CODE=$(curl -fsSL -w '%{http_code}' -o "$TMP_DIR/$TARBALL_NAME" "$DOWNLOAD_URL" 2>/dev/null || true)
        if [ ! -f "$TMP_DIR/$TARBALL_NAME" ] || [ ! -s "$TMP_DIR/$TARBALL_NAME" ]; then
            error "failed to download $DOWNLOAD_URL (HTTP $HTTP_CODE)"
        fi
    elif command -v wget > /dev/null 2>&1; then
        wget -q -O "$TMP_DIR/$TARBALL_NAME" "$DOWNLOAD_URL" || error "failed to download $DOWNLOAD_URL"
    else
        error "need 'curl' or 'wget' to download"
    fi

    # Download and verify checksum
    verify_checksum "$TMP_DIR/$TARBALL_NAME" "$TARBALL_NAME" "$CHECKSUM_URL" "$TMP_DIR"
}

# --- Verify checksum ---
verify_checksum() {
    FILE="$1"
    FILENAME="$2"
    CHECKSUMS_URL="$3"
    WORK_DIR="$4"

    CHECKSUMS_FILE="$WORK_DIR/SHA256SUMS"

    # Download checksums file
    if command -v curl > /dev/null 2>&1; then
        curl -fsSL -o "$CHECKSUMS_FILE" "$CHECKSUMS_URL" 2>/dev/null || {
            warn "could not download checksums file, skipping verification"
            return 0
        }
    elif command -v wget > /dev/null 2>&1; then
        wget -q -O "$CHECKSUMS_FILE" "$CHECKSUMS_URL" 2>/dev/null || {
            warn "could not download checksums file, skipping verification"
            return 0
        }
    else
        warn "no download tool available for checksums, skipping verification"
        return 0
    fi

    # Extract expected checksum
    EXPECTED=$(grep "$FILENAME" "$CHECKSUMS_FILE" | awk '{print $1}')
    if [ -z "$EXPECTED" ]; then
        warn "no checksum found for $FILENAME in SHA256SUMS, skipping verification"
        return 0
    fi

    # Compute actual checksum
    if command -v sha256sum > /dev/null 2>&1; then
        ACTUAL=$(sha256sum "$FILE" | awk '{print $1}')
    elif command -v shasum > /dev/null 2>&1; then
        ACTUAL=$(shasum -a 256 "$FILE" | awk '{print $1}')
    else
        warn "no sha256 tool available, skipping checksum verification"
        return 0
    fi

    if [ "$EXPECTED" != "$ACTUAL" ]; then
        error "checksum mismatch for $FILENAME\n  expected: $EXPECTED\n  actual:   $ACTUAL"
    fi

    success "Checksum verified."
}

# --- Install ---
install_binary() {
    mkdir -p "$INSTALL_DIR"

    printf "Extracting to %s...\n" "$INSTALL_DIR"
    tar xzf "$TMP_DIR/$TARBALL_NAME" -C "$INSTALL_DIR"
    chmod +x "$INSTALL_DIR/git-ai"

    # Remove macOS quarantine attribute if present
    if [ "$(uname -s)" = "Darwin" ]; then
        xattr -d com.apple.quarantine "$INSTALL_DIR/git-ai" 2>/dev/null || true
    fi
}

# --- Post-install ---
setup_path() {
    # Check if already on PATH
    case ":${PATH}:" in
        *":${INSTALL_DIR}:"*) return 0 ;;
    esac

    SHELL_NAME="$(basename "${SHELL:-/bin/sh}")"
    PROFILE=""

    case "$SHELL_NAME" in
        bash)
            if [ -f "$HOME/.bashrc" ]; then
                PROFILE="$HOME/.bashrc"
            elif [ -f "$HOME/.bash_profile" ]; then
                PROFILE="$HOME/.bash_profile"
            else
                PROFILE="$HOME/.bashrc"
            fi
            ;;
        zsh)
            PROFILE="$HOME/.zshrc"
            ;;
        fish)
            PROFILE="$HOME/.config/fish/config.fish"
            ;;
        *)
            PROFILE="$HOME/.profile"
            ;;
    esac

    if [ -n "$PROFILE" ]; then
        if [ -f "$PROFILE" ] && grep -qF "$INSTALL_DIR" "$PROFILE" 2>/dev/null; then
            return 0
        fi

        printf "\n# Added by git-ai installer\n" >> "$PROFILE"
        if [ "$SHELL_NAME" = "fish" ]; then
            mkdir -p "$(dirname "$PROFILE")"
            printf "fish_add_path -g \"%s\"\n" "$INSTALL_DIR" >> "$PROFILE"
        else
            printf "export PATH=\"%s:\$PATH\"\n" "$INSTALL_DIR" >> "$PROFILE"
        fi
        success "Added $INSTALL_DIR to PATH in $PROFILE"
    fi
}

run_git_ai_install() {
    printf "Running git-ai install...\n"
    if "$INSTALL_DIR/git-ai" install 2>/dev/null; then
        success "git-ai install completed successfully."
    else
        warn "git-ai install exited with non-zero status. You may need to run 'git-ai install' manually."
    fi
}

# --- Main ---
main() {
    need_cmd uname
    need_cmd tar
    need_cmd mktemp

    detect_platform
    get_version
    download_release
    install_binary
    setup_path
    run_git_ai_install

    printf "\n"
    success "git-ai has been installed to $INSTALL_DIR"
    printf "\n"

    # Show version
    if "$INSTALL_DIR/git-ai" --version > /dev/null 2>&1; then
        INSTALLED_VERSION=$("$INSTALL_DIR/git-ai" --version 2>&1)
        printf "Installed: %s\n" "$INSTALLED_VERSION"
    fi

    printf "\nRestart your shell or run:\n"
    printf "  export PATH=\"%s:\$PATH\"\n" "$INSTALL_DIR"
    printf "\n"
}

main "$@"
