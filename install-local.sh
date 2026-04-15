#!/bin/bash

set -euo pipefail
IFS=$'\n\t'

# ============================================================
# easylife-ai local offline installer
# Installs from binaries in the same directory as this script.
# No network access required.
# ============================================================

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
NC='\033[0m'

error() {
    echo -e "${RED}Error: $1${NC}" >&2
    exit 1
}

warn() {
    echo -e "${YELLOW}Warning: $1${NC}" >&2
}

success() {
    echo -e "${GREEN}$1${NC}"
}

# Directory containing this script (where the binaries live)
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Detect OS and architecture
OS=$(uname -s | tr '[:upper:]' '[:lower:]')
ARCH=$(uname -m)

case $ARCH in
    "x86_64")  ARCH="x64" ;;
    "aarch64"|"arm64") ARCH="arm64" ;;
    *) error "Unsupported architecture: $ARCH" ;;
esac

case $OS in
    "darwin") OS="macos" ;;
    "linux")  OS="linux" ;;
    *) error "Unsupported operating system: $OS" ;;
esac

BINARY_NAME="easylife-ai-${OS}-${ARCH}"
BINARY_PATH="${SCRIPT_DIR}/${BINARY_NAME}"

if [ ! -f "$BINARY_PATH" ]; then
    error "Binary not found: ${BINARY_PATH}\nMake sure ${BINARY_NAME} is in the same directory as this script."
fi

# Detect standard git (needed for git-og symlink and config)
detect_std_git() {
    local git_path=""
    if git_path=$(type -P git 2>/dev/null); then
        :
    else
        git_path=$(command -v git 2>/dev/null || true)
    fi
    if [ -z "$git_path" ]; then
        git_path=$(which git 2>/dev/null || true)
    fi
    if [ -n "$git_path" ] && [[ "$git_path" == *"git-ai"* ]]; then
        git_path=""
    fi
    if [ -z "$git_path" ]; then
        local cfg_json="$HOME/.git-ai/config.json"
        if [ -f "$cfg_json" ]; then
            local cfg_git_path
            cfg_git_path=$(sed -n 's/.*"git_path"[[:space:]]*:[[:space:]]*"\(.*\)".*/\1/p' "$cfg_json" | head -n1 || true)
            if [ -n "$cfg_git_path" ] && [[ "$cfg_git_path" != *"git-ai"* ]]; then
                if "$cfg_git_path" --version >/dev/null 2>&1; then
                    git_path="$cfg_git_path"
                fi
            fi
        fi
    fi
    if [ -z "$git_path" ]; then
        error "Could not detect a standard git binary on PATH. Please ensure Git is installed."
    fi
    echo "$git_path"
}

STD_GIT_PATH=$(detect_std_git)

INSTALL_DIR="$HOME/.git-ai/bin"
mkdir -p "$INSTALL_DIR"

echo "Installing easylife-ai from ${BINARY_PATH}..."
cp "$BINARY_PATH" "${INSTALL_DIR}/easylife-ai"
chmod +x "${INSTALL_DIR}/easylife-ai"

# Symlinks
ln -sf "${INSTALL_DIR}/easylife-ai" "${INSTALL_DIR}/git"
ln -sf "$STD_GIT_PATH" "${INSTALL_DIR}/git-og"

# Remove quarantine on macOS
if [ "$OS" = "macos" ]; then
    xattr -d com.apple.quarantine "${INSTALL_DIR}/easylife-ai" 2>/dev/null || true
fi

# ~/.local/bin symlink (non-fatal)
LOCAL_BIN_DIR="$HOME/.local/bin"
if mkdir -p "$LOCAL_BIN_DIR" 2>/dev/null; then
    ln -sf "${INSTALL_DIR}/easylife-ai" "${LOCAL_BIN_DIR}/easylife-ai" 2>/dev/null || true
fi

success "Installed to ${INSTALL_DIR}"

# Write config.json if not present
CONFIG_DIR="$HOME/.git-ai"
CONFIG_JSON_PATH="$CONFIG_DIR/config.json"
mkdir -p "$CONFIG_DIR"
if [ ! -f "$CONFIG_JSON_PATH" ]; then
    TMP_CFG="$CONFIG_JSON_PATH.tmp.$$"
    cat >"$TMP_CFG" <<EOF
{
  "git_path": "${STD_GIT_PATH}",
  "feature_flags": {
    "async_mode": true
  }
}
EOF
    mv -f "$TMP_CFG" "$CONFIG_JSON_PATH"
fi

# Install hooks
echo "Setting up IDE/agent hooks..."
if ! "${INSTALL_DIR}/easylife-ai" install-hooks; then
    warn "Failed to set up IDE/agent hooks. Run 'easylife-ai install-hooks' manually."
else
    success "IDE/agent hooks configured"
fi

# Detect shell configs and inject PATH
detect_all_shells() {
    local shells=""
    [ -f "$HOME/.bashrc" ]  && shells="${shells}bash|$HOME/.bashrc\n"
    [ -f "$HOME/.bash_profile" ] && [ -z "$(echo "$shells" | grep bash)" ] && shells="${shells}bash|$HOME/.bash_profile\n"
    [ -f "$HOME/.zshrc" ]   && shells="${shells}zsh|$HOME/.zshrc\n"
    [ -f "$HOME/.config/fish/config.fish" ] && shells="${shells}fish|$HOME/.config/fish/config.fish\n"
    if [ -z "$shells" ]; then
        local login_shell=""
        [ -n "${SHELL:-}" ] && login_shell=$(basename "$SHELL")
        case "$login_shell" in
            fish) shells="fish|$HOME/.config/fish/config.fish" ;;
            zsh)  shells="zsh|$HOME/.zshrc" ;;
            *)    shells="bash|$HOME/.bashrc" ;;
        esac
    fi
    printf '%b' "$shells" | sed '/^$/d'
}

SHELLS_CONFIGURED=""
SHELLS_ALREADY_CONFIGURED=""

while IFS='|' read -r shell_name config_file; do
    [ -z "$shell_name" ] && continue
    if [ "$shell_name" = "fish" ]; then
        path_cmd="fish_add_path -g \"$INSTALL_DIR\""
        mkdir -p "$(dirname "$config_file")" 2>/dev/null || true
    else
        path_cmd="export PATH=\"$INSTALL_DIR:\$PATH\""
    fi
    touch "$config_file"
    if ! grep -qsF "$INSTALL_DIR" "$config_file"; then
        echo "" >> "$config_file"
        echo "# Added by easylife-ai installer on $(date)" >> "$config_file"
        echo "$path_cmd" >> "$config_file"
        SHELLS_CONFIGURED="${SHELLS_CONFIGURED}${shell_name}|${config_file}\n"
    else
        SHELLS_ALREADY_CONFIGURED="${SHELLS_ALREADY_CONFIGURED}${shell_name}|${config_file}\n"
    fi
done <<< "$(detect_all_shells)"

if [ -n "$SHELLS_CONFIGURED" ]; then
    echo ""
    echo "Updated shell configurations:"
    printf '%b' "$SHELLS_CONFIGURED" | while IFS='|' read -r shell_name config_file; do
        [ -z "$shell_name" ] && continue
        success "  ✓ $config_file"
    done
    echo ""
    echo "To apply changes immediately:"
    printf '%b' "$SHELLS_CONFIGURED" | while IFS='|' read -r shell_name config_file; do
        [ -z "$shell_name" ] && continue
        echo "  - source $config_file"
    done
fi

if [ -n "$SHELLS_ALREADY_CONFIGURED" ]; then
    echo ""
    echo "Already configured (no changes needed):"
    printf '%b' "$SHELLS_ALREADY_CONFIGURED" | while IFS='|' read -r shell_name config_file; do
        [ -z "$shell_name" ] && continue
        echo "  ✓ $config_file"
    done
fi

echo ""
echo -e "${YELLOW}Close and reopen your terminal and IDE sessions to use easylife-ai.${NC}"

INSTALLED_VERSION=$("${INSTALL_DIR}/easylife-ai" --version 2>&1 || echo "unknown")
echo "Installed ${INSTALLED_VERSION}"
