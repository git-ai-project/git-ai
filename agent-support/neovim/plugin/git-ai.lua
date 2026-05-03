-- Shim loaded automatically by Neovim on startup.
-- Calls setup() with default options.
if vim.g.loaded_git_ai then
  return
end
vim.g.loaded_git_ai = 1
require('git-ai').setup()
