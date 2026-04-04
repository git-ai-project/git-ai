#!/bin/bash

set -euo pipefail

# Parse arguments
BUILD_TYPE="debug"
if [[ "$#" -gt 0 && "$1" == "--release" ]]; then
    BUILD_TYPE="release"
fi

# Verify ~/.git-ai/bin and ~/.git-ai/config.json exist
if [[ ! -d "$HOME/.git-ai/bin" ]] || [[ ! -f "$HOME/.git-ai/config.json" ]]; then
    echo "Setting up ~/.git-ai..."
    curl -sSL https://usegitai.com/install.sh | bash
fi

# Build the binary
echo "Building $BUILD_TYPE binary..."
if [[ "$BUILD_TYPE" == "release" ]]; then
    cargo build --release
else
    cargo build
fi

# Copy binary to ~/.git-ai/bin/git-ai
echo "Installing binary to ~/.git-ai/bin/git-ai..."
cp "target/$BUILD_TYPE/git-ai" "$HOME/.git-ai/bin/git-ai"

# Run install hooks
echo "Running install hooks..."
~/.git-ai/bin/git-ai install

echo "Done!"
