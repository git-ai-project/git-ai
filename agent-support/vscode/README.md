# git-ai Extension for VS Code & Cursor

A VS Code and Cursor extension that tracks AI-generated code using [git-ai](https://github.com/git-ai-project/git-ai?tab=readme-ov-file#quick-start).

## Manual Install

The [git-ai quickstart](https://github.com/git-ai-project/git-ai?tab=readme-ov-file#quick-start) install script should automatically install both the Cursor and VS Code extensions automatically, however, if that didn't work or you'd like to install the extension manually, please follow the instructions below:

### Cursor

1. **Install the extension** We recommend installing the Cursor extension by searching for `git-ai` in the extensions tab. To open the extensions tab, you can press `Cmd+Shift+P` and search for `>Extensions: Install Extensions`.
2. **Install [`git-ai`](https://github.com/git-ai-project/git-ai)** Follow the `git-ai` installation [instructions](https://github.com/git-ai-project/git-ai?tab=readme-ov-file#quick-start) for your platform.
3. **Restart VS Code**

### VS Code

1. **Install the extension** We recommend installing from the [VS Code Extension marketplace](https://marketplace.visualstudio.com/items?itemName=git-ai.git-ai-vscode)
2. **Install [`git-ai`](https://github.com/git-ai-project/git-ai)** Follow the `git-ai` installation [instructions](https://github.com/git-ai-project/git-ai?tab=readme-ov-file#quick-start) for your platform.
3. **Restart VS Code**

### Debug logging

You can enable toast messages from the extension when it calls checkpoints to get a feel for the effectiveness of the heuristics add this option to your settings:

```json
"gitai.enableCheckpointLogging": true
```

### AI tab tracking (experimental)

Adds support for tracking AI tab-completion insertions

- **Setting**: `gitai.experiments.aiTabTracking` (default: `false`)
- **Notes**:
  - Requires restarting VS Code/Cursor after changing the setting.
  - On activation, you'll see a notification: "git-ai: AI tab tracking is enabled (experimental)".
  - Requires the latest release of the `git-ai` CLI

Enable it in Settings or add to your `settings.json`:

```json
"gitai.experiments.aiTabTracking": true
```

## License

MIT
