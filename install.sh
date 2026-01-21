#!/bin/bash

set -euo pipefail
IFS=$'\n\t'

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
NC='\033[0m' # No Color

# GitHub repository details
# Replaced during release builds with the actual repository (e.g., "acunniffe/git-ai")
# When set to __REPO_PLACEHOLDER__, defaults to "acunniffe/git-ai"
REPO="__REPO_PLACEHOLDER__"
if [ "$REPO" = "__REPO_PLACEHOLDER__" ]; then
    REPO="acunniffe/git-ai"
fi

# Version placeholder - replaced during release builds with actual version (e.g., "v1.0.24")
# When set to __VERSION_PLACEHOLDER__, defaults to "latest"
PINNED_VERSION="__VERSION_PLACEHOLDER__"

# Embedded checksums - replaced during release builds with actual SHA256 checksums
# Format: "hash  filename|hash  filename|..." (pipe-separated)
# When set to __CHECKSUMS_PLACEHOLDER__, checksum verification is skipped
EMBEDDED_CHECKSUMS="__CHECKSUMS_PLACEHOLDER__"

# Function to print error messages
error() {
    echo -e "${RED}Error: $1${NC}" >&2
    exit 1
}

warn() {
    echo -e "${YELLOW}Warning: $1${NC}" >&2
}

# Function to print success messages
success() {
    echo -e "${GREEN}$1${NC}"
}

# Function to verify checksum of downloaded binary
verify_checksum() {
    local file="$1"
    local binary_name="$2"

    # Skip verification if no checksums are embedded
    if [ "$EMBEDDED_CHECKSUMS" = "__CHECKSUMS_PLACEHOLDER__" ]; then
        return 0
    fi

    # Extract expected checksum for this binary
    local expected=""
    local old_ifs="$IFS"
    IFS='|' read -ra CHECKSUM_ENTRIES <<< "$EMBEDDED_CHECKSUMS"
    IFS="$old_ifs"
    for entry in "${CHECKSUM_ENTRIES[@]}"; do
        if [[ "$entry" =~ ^[[:xdigit:]]+[[:space:]]+$binary_name$ ]]; then
            expected=$(echo "$entry" | awk '{print $1}')
            break
        fi
    done

    if [ -z "$expected" ]; then
        error "No checksum found for $binary_name"
    fi

    # Calculate actual checksum
    local actual=""
    if command -v sha256sum >/dev/null 2>&1; then
        actual=$(sha256sum "$file" | awk '{print $1}')
    elif command -v shasum >/dev/null 2>&1; then
        actual=$(shasum -a 256 "$file" | awk '{print $1}')
    else
        warn "Neither sha256sum nor shasum available, skipping checksum verification"
        return 0
    fi

    if [ "$expected" != "$actual" ]; then
        rm -f "$file" 2>/dev/null || true
        error "Checksum verification failed for $binary_name\nExpected: $expected\nActual:   $actual"
    fi

    success "Checksum verified for $binary_name"
}

# Function to detect shell and generate alias command
detect_shell() {
    local shell_name=""
    local config_file=""
    
    # Check for zsh first (macOS default)
    if [ -f "$HOME/.zshrc" ]; then
        shell_name="zsh"
        config_file="$HOME/.zshrc"
    # Check for bash
    elif [ -f "$HOME/.bashrc" ] || [ -f "$HOME/.bash_profile" ]; then
        shell_name="bash"
        config_file="$HOME/.bashrc"
    else
        # Fallback - try to detect from environment
        if [ -n "$ZSH_VERSION" ]; then
            shell_name="zsh"
            config_file="$HOME/.zshrc"
        elif [ -n "$BASH_VERSION" ]; then
            shell_name="bash"
            config_file="$HOME/.bashrc"
        else
            shell_name="unknown"
            config_file=""
        fi
    fi
    
    echo "$shell_name|$config_file"
}

detect_std_git() {
    local git_path=""

    # Prefer the actual executable path, ignoring aliases and functions
    if git_path=$(type -P git 2>/dev/null); then
        :
    else
        git_path=$(command -v git 2>/dev/null || true)
    fi

    # Last resort
    if [ -z "$git_path" ]; then
        git_path=$(which git 2>/dev/null || true)
    fi

	# Ensure we never return a path for git that contains git-ai (recursive)
	if [ -n "$git_path" ] && [[ "$git_path" == *"git-ai"* ]]; then
		git_path=""
	fi

    # If detection failed or was our own shim, try to recover from saved config
    if [ -z "$git_path" ]; then
        local cfg_json="$HOME/.git-ai/config.json"
        if [ -f "$cfg_json" ]; then
            # Extract git_path value without jq
            local cfg_git_path
            cfg_git_path=$(sed -n 's/.*"git_path"[[:space:]]*:[[:space:]]*"\(.*\)".*/\1/p' "$cfg_json" | head -n1 || true)
            if [ -n "$cfg_git_path" ] && [[ "$cfg_git_path" != *"git-ai"* ]]; then
                if "$cfg_git_path" --version >/dev/null 2>&1; then
                    git_path="$cfg_git_path"
                fi
            fi
        fi
    fi

    # Fail if we couldn't find a standard git
    if [ -z "$git_path" ]; then
        error "Could not detect a standard git binary on PATH. Please ensure you have Git installed and available on your PATH. If you believe this is a bug with the installer, please file an issue at https://github.com/acunniffe/git-ai/issues."
    fi

    # Verify detected git is usable
    if ! "$git_path" --version >/dev/null 2>&1; then
        error "Detected git at $git_path is not usable (--version failed). Please ensure you have Git installed and available on your PATH. If you believe this is a bug with the installer, please file an issue at https://github.com/acunniffe/git-ai/issues."
    fi

    echo "$git_path"
}

# Detect shell and get alias information
SHELL_INFO=$(detect_shell)
SHELL_NAME=$(echo "$SHELL_INFO" | cut -d'|' -f1)
CONFIG_FILE=$(echo "$SHELL_INFO" | cut -d'|' -f2)
STD_GIT_PATH=$(detect_std_git)

# Detect OS and architecture
OS=$(uname -s | tr '[:upper:]' '[:lower:]')
ARCH=$(uname -m)

# Map architecture to binary name
case $ARCH in
    "x86_64")
        ARCH="x64"
        ;;
    "aarch64"|"arm64")
        ARCH="arm64"
        ;;
    *)
        error "Unsupported architecture: $ARCH"
        ;;
esac

# Map OS to binary name
case $OS in
    "darwin")
        OS="macos"
        ;;
    "linux")
        OS="linux"
        ;;
    *)
        error "Unsupported operating system: $OS"
        ;;
esac

# Determine binary name
BINARY_NAME="git-ai-${OS}-${ARCH}"

# Determine release tag
# Priority: 1. Pinned version (for release builds), 2. Environment variable, 3. "latest"
if [ "$PINNED_VERSION" != "__VERSION_PLACEHOLDER__" ]; then
    # Version-pinned install script from a release
    RELEASE_TAG="$PINNED_VERSION"
    DOWNLOAD_URL="https://usegitai.com/worker/releases/download/${RELEASE_TAG}/${BINARY_NAME}"
elif [ -n "${GIT_AI_RELEASE_TAG:-}" ] && [ "${GIT_AI_RELEASE_TAG:-}" != "latest" ]; then
    # Environment variable override
    RELEASE_TAG="$GIT_AI_RELEASE_TAG"
    DOWNLOAD_URL="https://usegitai.com/worker/releases/download/${RELEASE_TAG}/${BINARY_NAME}"
else
    # Default to latest
    RELEASE_TAG="latest"
    DOWNLOAD_URL="https://usegitai.com/worker/releases/download/latest/${BINARY_NAME}"
fi

# Install into the user's bin directory ~/.git-ai/bin
INSTALL_DIR="$HOME/.git-ai/bin"

# Create directory if it doesn't exist
mkdir -p "$INSTALL_DIR"

# Download and install
echo "Downloading git-ai (release: ${RELEASE_TAG})..."
TMP_FILE="${INSTALL_DIR}/git-ai.tmp.$$"
if ! curl --fail --location --silent --show-error -o "$TMP_FILE" "$DOWNLOAD_URL"; then
    rm -f "$TMP_FILE" 2>/dev/null || true
    error "Failed to download binary (HTTP error)"
fi

# Basic validation: ensure file is not empty
if [ ! -s "$TMP_FILE" ]; then
    rm -f "$TMP_FILE" 2>/dev/null || true
    error "Downloaded file is empty"
fi

# Verify checksum if embedded (release builds only)
verify_checksum "$TMP_FILE" "$BINARY_NAME"

mv -f "$TMP_FILE" "${INSTALL_DIR}/git-ai"

# Make executable
chmod +x "${INSTALL_DIR}/git-ai"
# Symlink git to git-ai
ln -sf "${INSTALL_DIR}/git-ai" "${INSTALL_DIR}/git"

# Symlink git-og to the detected standard git path
ln -sf "$STD_GIT_PATH" "${INSTALL_DIR}/git-og"

# Create libexec/git-core symlink structure
# This is needed because Git expects to find executables in libexec/git-core/
LIBEXEC_DIR="$HOME/.git-ai/libexec/git-core"
mkdir -p "$LIBEXEC_DIR"
ln -sf "../../bin/git-ai" "${LIBEXEC_DIR}/git"

# Remove quarantine attribute on macOS
if [ "$OS" = "macos" ]; then
    xattr -d com.apple.quarantine "${INSTALL_DIR}/git-ai" 2>/dev/null || true
fi

PATH_CMD="export PATH=\"$INSTALL_DIR:\$PATH\""

success "Successfully installed git-ai into ${INSTALL_DIR}"
success "You can now run 'git-ai' from your terminal"

# Print installed version
INSTALLED_VERSION=$(${INSTALL_DIR}/git-ai --version 2>&1 || echo "unknown")
echo "Installed git-ai ${INSTALLED_VERSION}"

# Install hooks
echo "Setting up IDE/agent hooks..."
if ! ${INSTALL_DIR}/git-ai install-hooks; then
    warn "Warning: Failed to set up IDE/agent hooks. Please try running 'git-ai install-hooks' manually."
else
    success "Successfully set up IDE/agent hooks"
fi

# Write JSON config at ~/.git-ai/config.json (only if it doesn't exist)
CONFIG_DIR="$HOME/.git-ai"
CONFIG_JSON_PATH="$CONFIG_DIR/config.json"
mkdir -p "$CONFIG_DIR"

if [ ! -f "$CONFIG_JSON_PATH" ]; then
    TMP_CFG="$CONFIG_JSON_PATH.tmp.$$"
    cat >"$TMP_CFG" <<EOF
{
  "git_path": "${STD_GIT_PATH}"
}
EOF
    mv -f "$TMP_CFG" "$CONFIG_JSON_PATH"
fi

# Add to PATH automatically if not already there
if [[ ":$PATH:" != *"$INSTALL_DIR"* ]]; then
    if [ -n "$CONFIG_FILE" ]; then
        # Ensure config file exists
        touch "$CONFIG_FILE"
        # Append PATH update if not already present
        if ! grep -qsF "$INSTALL_DIR" "$CONFIG_FILE"; then
            echo "" >> "$CONFIG_FILE"
            echo "# Added by git-ai installer on $(date)" >> "$CONFIG_FILE"
            echo "$PATH_CMD" >> "$CONFIG_FILE"
        fi
        success "Updated ${CONFIG_FILE} to include ${INSTALL_DIR} in PATH"
        echo "Restart your shell or run: source \"$CONFIG_FILE\""
    else
        echo "Could not detect your shell config file."
        echo "Please add the following line(s) to your shell config and restart:"
        echo "$PATH_CMD"
    fi
fi

echo -e "${YELLOW}Close and reopen your terminal and IDE sessions to use git-ai.${NC}"
