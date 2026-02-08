#!/usr/bin/env bash

set -euo pipefail
IFS=$'\n\t'

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
SHELL_NAME="${SHELL_NAME:-bash}"

if ! command -v "$SHELL_NAME" >/dev/null 2>&1; then
    echo "Required shell '$SHELL_NAME' is not available." >&2
    exit 1
fi

TEST_ROOT="$(mktemp -d)"
chmod 700 "$TEST_ROOT"
cleanup() {
    rm -rf "$TEST_ROOT"
}
trap cleanup EXIT

extract_version() {
    local output="$1"
    local match
    match="$(echo "$output" | grep -Eo '[0-9]+\.[0-9]+\.[0-9]+[^[:space:]]*' | head -n1)" || return 1
    if [ -z "$match" ]; then
        return 1
    fi
    printf '%s' "$match"
}

export HOME="$TEST_ROOT/home"
mkdir -p "$HOME"

export PATH="$TEST_ROOT/bin:$PATH"

mkdir -p "$TEST_ROOT/bin"
cat > "$TEST_ROOT/bin/claude" <<'EOF'
#!/usr/bin/env bash
echo "2.0.0"
EOF
chmod +x "$TEST_ROOT/bin/claude"

INSTALL_DIR="$HOME/.git-ai/bin"

case "$SHELL_NAME" in
    bash)
        CONFIG_FILE="$HOME/.bashrc"
        EXPECTED_PATH_LINE="export PATH=\"$INSTALL_DIR:\$PATH\""
        SHELL_COMMAND="bash -lc"
        SHELL_CHECK="source \"$CONFIG_FILE\"; command -v git-ai >/dev/null; git-ai --version >/dev/null"
        ;;
    zsh)
        CONFIG_FILE="$HOME/.zshrc"
        EXPECTED_PATH_LINE="export PATH=\"$INSTALL_DIR:\$PATH\""
        SHELL_COMMAND="zsh -lc"
        SHELL_CHECK="source \"$CONFIG_FILE\"; command -v git-ai >/dev/null; git-ai --version >/dev/null"
        ;;
    fish)
        CONFIG_FILE="$HOME/.config/fish/config.fish"
        EXPECTED_PATH_LINE="fish_add_path -g \"$INSTALL_DIR\""
        SHELL_COMMAND="fish -c"
        SHELL_CHECK="source \"$CONFIG_FILE\"; type -q git-ai; git-ai --version >/dev/null"
        ;;
    *)
        echo "Unsupported shell: $SHELL_NAME" >&2
        exit 1
        ;;
esac

mkdir -p "$(dirname "$CONFIG_FILE")"
touch "$CONFIG_FILE"

export SHELL
SHELL="$(command -v "$SHELL_NAME")"

chmod +x "$REPO_ROOT/install.sh"

"$REPO_ROOT/install.sh"

if [ ! -x "$INSTALL_DIR/git-ai" ]; then
    echo "git-ai binary not found at $INSTALL_DIR/git-ai" >&2
    exit 1
fi

VERSION_OUTPUT="$("$INSTALL_DIR/git-ai" --version)"
if ! VERSION="$(extract_version "$VERSION_OUTPUT")"; then
    echo "Unable to parse version from: $VERSION_OUTPUT" >&2
    exit 1
fi

if ! grep -Fqs "$INSTALL_DIR" "$CONFIG_FILE"; then
    echo "PATH was not updated in $CONFIG_FILE" >&2
    exit 1
fi

if ! grep -Fqs "$EXPECTED_PATH_LINE" "$CONFIG_FILE"; then
    echo "Expected PATH line missing from $CONFIG_FILE" >&2
    exit 1
fi

PATH_LINE_COUNT="$(grep -F "$INSTALL_DIR" "$CONFIG_FILE" | wc -l | tr -d ' ')"
if [ "$PATH_LINE_COUNT" -ne 1 ]; then
    echo "PATH entry duplicated in $CONFIG_FILE" >&2
    exit 1
fi

CLAUDE_SETTINGS="$HOME/.claude/settings.json"
CLAUDE_HOOK_ARGS="checkpoint claude --hook-input stdin"
if [ ! -f "$CLAUDE_SETTINGS" ]; then
    echo "Claude settings.json not created at $CLAUDE_SETTINGS" >&2
    exit 1
fi

if ! grep -Fqs "$CLAUDE_HOOK_ARGS" "$CLAUDE_SETTINGS"; then
    echo "Claude hooks not configured in $CLAUDE_SETTINGS" >&2
    exit 1
fi

# Match an absolute git-ai command with whitespace-separated arguments in Claude settings.
CLAUDE_HOOK_ARGS_REGEX="${CLAUDE_HOOK_ARGS// /[[:space:]]+}"
CLAUDE_HOOK_REGEX="[^[:space:]]+/git-ai[[:space:]]+${CLAUDE_HOOK_ARGS_REGEX}"
if ! grep -Eq "$CLAUDE_HOOK_REGEX" "$CLAUDE_SETTINGS"; then
    echo "git-ai command missing in Claude hooks config" >&2
    exit 1
fi

$SHELL_COMMAND "$SHELL_CHECK"

OVERRIDE_TAG="v$VERSION"
OVERRIDE_OUTPUT="$(GIT_AI_RELEASE_TAG="$OVERRIDE_TAG" "$REPO_ROOT/install.sh")"
if ! echo "$OVERRIDE_OUTPUT" | grep -Fqs "release: $OVERRIDE_TAG"; then
    echo "Release tag not found in override output" >&2
    exit 1
fi

OVERRIDE_VERSION_OUTPUT="$("$INSTALL_DIR/git-ai" --version)"
if ! OVERRIDE_VERSION="$(extract_version "$OVERRIDE_VERSION_OUTPUT")"; then
    echo "Unable to parse version from override install: $OVERRIDE_VERSION_OUTPUT" >&2
    exit 1
fi

if [ "$OVERRIDE_VERSION" != "$VERSION" ]; then
    echo "Override install version mismatch: $OVERRIDE_VERSION (expected $VERSION)" >&2
    exit 1
fi
