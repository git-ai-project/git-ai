# git-ai Plugin for OpenCode

A plugin that integrates [git-ai](https://github.com/git-ai-project/git-ai) with [OpenCode](https://opencode.ai) to automatically track AI-generated code.

## Overview

This plugin hooks into OpenCode's tool execution lifecycle to create checkpoints that mark code changes as either human or AI-authored. It uses the `tool.execute.before` and `tool.execute.after` events to:

1. Create a human checkpoint before AI edits (marking any intermediate changes as human-authored)
2. Create an AI checkpoint after AI edits (marking the changes as AI-authored with model information)

## Installation

The plugin is automatically installed by `git-ai install-hooks`.

Build `git-ai` (`cargo build`) and then run the `git-ai install-hooks` or `cargo run -- install-hooks` command to test the entire flow of installing and using the plugin.

## Requirements

- [git-ai](https://github.com/git-ai-project/git-ai) must be installed and available in PATH
- [OpenCode](https://opencode.ai) with plugin support

## How It Works

The plugin intercepts file editing operations (`edit` and `write` tools) and:

1. **Before AI edit**: Creates a human checkpoint to mark any changes since the last checkpoint as human-authored
2. **After AI edit**: Creates an AI checkpoint with:
   - Model information (provider/model ID)
   - Session/conversation ID
   - List of edited file paths

If `git-ai` is not installed or the file is not in a git repository, the plugin gracefully skips checkpoint creation without breaking OpenCode functionality.

## Development

### Type Checking

Run type checking:
```bash
yarn type-check
```

### Dependencies

Install dependencies:
```bash
yarn install
```

## See Also

- [git-ai Documentation](https://github.com/git-ai-project/git-ai)
- [OpenCode Plugin Documentation](https://opencode.ai/docs/plugins/)

