# git-ai Neovim Plugin

Fires `git-ai checkpoint known_human --hook-input stdin` on every file save,
debounced 500 ms per git repository root.

## Requirements

- Neovim 0.8+
- `git-ai` installed at `~/.git-ai/bin/git-ai` (or available on `$PATH`)

## Installation

### lazy.nvim (recommended)

```lua
{ 'git-ai-project/git-ai', opts = {} }
```

### packer.nvim

```lua
use {
  'git-ai-project/git-ai',
  config = function() require('git-ai').setup() end,
}
```

### Native (`~/.config/nvim/init.lua`)

```bash
# Copy or symlink the plugin directory into your Neovim runtime path
cp -r agent-support/neovim ~/.local/share/nvim/site/pack/git-ai/start/git-ai
```

Or add to `init.lua`:

```lua
vim.opt.rtp:prepend('/path/to/agent-support/neovim')
require('git-ai').setup()
```

## Configuration

```lua
require('git-ai').setup({
  enabled = true,  -- set to false to disable (default: true)
})
```

## Disabling at runtime

```vim
:let g:git_ai_enabled = v:false
```
