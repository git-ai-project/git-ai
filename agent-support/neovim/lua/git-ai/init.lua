--- git-ai known_human checkpoint plugin for Neovim
--- Fires `git-ai checkpoint known_human --hook-input stdin` after each file save,
--- debounced 500ms per git repository root.
---
--- Setup (lazy.nvim):
---   { 'git-ai-project/git-ai', opts = {} }
---
--- Setup (packer.nvim):
---   use { 'git-ai-project/git-ai', config = function() require('git-ai').setup() end }
---
--- Setup (native, ~/.config/nvim/init.lua):
---   require('git-ai').setup()    -- after adding plugin dir to rtp

local M = {}

local uv = vim.uv or vim.loop  -- vim.uv available since Neovim 0.9

--- Per-repo-root state
local timers = {}   -- [repo_root] -> uv.timer
local pending = {}  -- [repo_root] -> { [path] = content }

--- Cache for find_repo_root results to avoid blocking systemlist calls per save.
local _repo_root_cache = {}  -- [dir] -> root_string or false

--- Resolve the git-ai binary path.
local function git_ai_bin()
  local home = vim.fn.expand('~')
  local candidates = {
    home .. '/.git-ai/bin/git-ai',
    home .. '/.git-ai/bin/git-ai.exe',
  }
  for _, p in ipairs(candidates) do
    if vim.fn.executable(p) == 1 then
      return p
    end
  end
  return 'git-ai'
end

--- Find the nearest .git directory walking up from `file`.
--- Returns the repo root path, or nil.
local function find_repo_root(file)
  local dir = vim.fn.fnamemodify(file, ':h')
  if dir == '' then return nil end
  if _repo_root_cache[dir] ~= nil then
    return _repo_root_cache[dir] or nil  -- false means "no repo"
  end
  local result = vim.fn.systemlist({'git', '-C', dir, 'rev-parse', '--show-toplevel'})
  local root = (vim.v.shell_error == 0 and #result > 0 and result[1] ~= '') and result[1] or false
  _repo_root_cache[dir] = root
  return root or nil
end

--- Fire the checkpoint for `root` with all accumulated pending files.
local function fire(root)
  local files = pending[root]
  if not files or next(files) == nil then return end
  pending[root] = {}

  local paths = {}
  for p in pairs(files) do table.insert(paths, p) end

  local v = vim.version()
  local editor_version = string.format('%d.%d.%d', v.major, v.minor, v.patch)

  local payload = vim.json.encode({
    editor            = 'neovim',
    editor_version    = editor_version,
    extension_version = '1.0.0',
    cwd               = root,
    edited_filepaths  = paths,
    dirty_files       = files,
  })

  local bin = git_ai_bin()
  local stdin_pipe = uv.new_pipe(false)
  local handle
  handle = uv.spawn(bin, {
    args  = {'checkpoint', 'known_human', '--hook-input', 'stdin'},
    stdio = {stdin_pipe, nil, nil},
    cwd   = root,
  }, function(code, _)
    handle:close()
    stdin_pipe:close()
    if code ~= 0 then
      vim.schedule(function()
        vim.notify('[git-ai] checkpoint known_human exited with code ' .. tostring(code), vim.log.levels.DEBUG)
      end)
    end
  end)

  if not handle then
    stdin_pipe:close()
    return
  end

  stdin_pipe:write(payload, function()
    stdin_pipe:shutdown(function() end)
  end)
end

--- BufWritePost callback.
local function on_save(args)
  if vim.g.git_ai_enabled == false then return end

  local file = vim.api.nvim_buf_get_name(args.buf)
  if file == '' then return end

  -- Skip git-internal paths
  if file:find('[/\\]%.git[/\\]') then return end

  local root = find_repo_root(file)
  if not root then return end

  -- Collect buffer content
  local lines = vim.api.nvim_buf_get_lines(args.buf, 0, -1, false)
  local content = table.concat(lines, '\n')

  if not pending[root] then pending[root] = {} end
  pending[root][file] = content

  -- Cancel existing debounce and start a new 500ms timer
  if timers[root] then
    timers[root]:stop()
    -- Reuse the same timer object
    timers[root]:start(500, 0, vim.schedule_wrap(function() fire(root) end))
  else
    local t = uv.new_timer()
    timers[root] = t
    t:start(500, 0, vim.schedule_wrap(function() fire(root) end))
  end
end

--- Setup the plugin. Call this once during Neovim startup.
--- @param opts table|nil Optional config. Pass { enabled = false } to disable.
function M.setup(opts)
  opts = opts or {}
  if opts.enabled == false then
    vim.g.git_ai_enabled = false
    return
  end
  vim.g.git_ai_enabled = true

  vim.api.nvim_create_autocmd('BufWritePost', {
    group    = vim.api.nvim_create_augroup('GitAiKnownHuman', {clear = true}),
    callback = on_save,
  })
end

return M
