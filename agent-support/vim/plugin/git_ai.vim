" git-ai known_human checkpoint plugin for Vim
" Fires `git-ai checkpoint known_human --hook-input stdin` after each file save.
" Debounced 500ms per git repository root.
"
" Installation (native packages, Vim 8+):
"   mkdir -p ~/.vim/pack/git-ai/start/git-ai
"   cp -r /path/to/agent-support/vim/* ~/.vim/pack/git-ai/start/git-ai/
"
" Installation (vim-plug):
"   Plug 'git-ai-project/git-ai', {'rtp': 'agent-support/vim'}
"
" Installation (Vundle):
"   Plugin 'git-ai-project/git-ai'
"   (add 'set runtimepath+=~/.vim/bundle/git-ai/agent-support/vim' to .vimrc)

if exists('g:loaded_git_ai') | finish | endif
let g:loaded_git_ai = 1

" Set to 0 to disable: let g:git_ai_enabled = 0
if !exists('g:git_ai_enabled')
  let g:git_ai_enabled = 1
endif

" Internal state: per repo root timers and pending files
let s:debounce_timers = {}
let s:pending_files = {}

function! s:GitAiBin() abort
  " Check known install locations first
  let l:candidates = [
    \ expand('~/.git-ai/bin/git-ai'),
    \ expand('~/.git-ai/bin/git-ai.exe'),
    \ ]
  for l:c in l:candidates
    if executable(l:c)
      return l:c
    endif
  endfor
  return 'git-ai'
endfunction

function! s:FindRepoRoot(file) abort
  let l:dir = fnamemodify(a:file, ':h')
  if l:dir ==# ''
    return ''
  endif
  let l:result = systemlist(['git', '-C', l:dir, 'rev-parse', '--show-toplevel'])
  if v:shell_error != 0 || empty(l:result)
    return ''
  endif
  return trim(l:result[0])
endfunction

function! s:BuildJsonPayload(root, files) abort
  " Build JSON manually — no external dependencies
  let l:paths_json = '['
  let l:dirty_json = '{'
  let l:first = 1
  for [l:path, l:content] in items(a:files)
    if !l:first
      let l:paths_json .= ','
      let l:dirty_json .= ','
    endif
    let l:first = 0
    let l:escaped_path = substitute(l:path, '\\', '\\\\', 'g')
    let l:escaped_path = substitute(l:escaped_path, '"', '\\"', 'g')
    let l:escaped_content = substitute(l:content, '\\', '\\\\', 'g')
    let l:escaped_content = substitute(l:escaped_content, '"', '\\"', 'g')
    let l:escaped_content = substitute(l:escaped_content, "\n", '\\n', 'g')
    let l:escaped_content = substitute(l:escaped_content, "\r", '\\r', 'g')
    let l:paths_json .= '"' . l:escaped_path . '"'
    let l:dirty_json .= '"' . l:escaped_path . '":"' . l:escaped_content . '"'
  endfor
  let l:paths_json .= ']'
  let l:dirty_json .= '}'
  let l:version = string(v:version)
  return '{"editor":"vim","editor_version":"' . l:version .
    \ '","extension_version":"1.0.0","cwd":"' .
    \ substitute(a:root, '"', '\\"', 'g') .
    \ '","edited_filepaths":' . l:paths_json .
    \ ',"dirty_files":' . l:dirty_json . '}'
endfunction

function! s:FireCheckpoint(root) abort
  if !has_key(s:pending_files, a:root)
    return
  endif
  let l:files = s:pending_files[a:root]
  unlet s:pending_files[a:root]
  if empty(l:files)
    return
  endif

  let l:payload = s:BuildJsonPayload(a:root, l:files)
  let l:bin = s:GitAiBin()
  let l:cmd = [l:bin, 'checkpoint', 'known_human', '--hook-input', 'stdin']

  if has('job') && has('channel')
    " Vim 8+ async job API
    let l:opts = {
      \ 'in_io': 'pipe',
      \ 'out_io': 'null',
      \ 'err_io': 'null',
      \ }
    let l:job = job_start(l:cmd, l:opts)
    if job_status(l:job) ==# 'run'
      let l:chan = job_getchannel(l:job)
      call ch_sendraw(l:chan, l:payload)
      call ch_close_in(l:chan)
    endif
  else
    " Fallback: synchronous (Vim < 8.0)
    let l:shell_cmd = join(map(copy(l:cmd), 'shellescape(v:val)'), ' ')
    call system(l:shell_cmd, l:payload)
  endif
endfunction

function! s:OnSave() abort
  if !g:git_ai_enabled
    return
  endif
  let l:file = expand('<afile>:p')
  if l:file ==# ''
    return
  endif
  " Skip git-internal files
  if l:file =~# '[/\\]\.git[/\\]'
    return
  endif
  let l:root = s:FindRepoRoot(l:file)
  if l:root ==# ''
    return
  endif

  " Read current buffer content
  let l:bufnr = bufnr(l:file)
  if l:bufnr > 0
    let l:content = join(getbufline(l:bufnr, 1, '$'), "\n")
  else
    " File closed within debounce window — read from disk
    let l:lines = readfile(l:file)
    let l:content = join(l:lines, "\n")
  endif

  if !has_key(s:pending_files, l:root)
    let s:pending_files[l:root] = {}
  endif
  let s:pending_files[l:root][l:file] = l:content

  " Cancel existing timer and start a new 500ms debounce
  if has_key(s:debounce_timers, l:root) && s:debounce_timers[l:root] >= 0
    call timer_stop(s:debounce_timers[l:root])
  endif

  if has('timers')
    let l:root_copy = l:root
    let s:debounce_timers[l:root] = timer_start(500,
      \ {-> s:FireCheckpoint(l:root_copy)})
  else
    " No timer support (Vim < 8.0): fire synchronously after a short delay
    call s:FireCheckpoint(l:root)
  endif
endfunction

augroup git_ai_known_human
  autocmd!
  autocmd BufWritePost * call s:OnSave()
augroup END
