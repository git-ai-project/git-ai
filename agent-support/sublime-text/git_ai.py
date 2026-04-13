"""
git-ai Sublime Text plugin — known_human checkpoint

Fires `git-ai checkpoint known_human --hook-input stdin` after each file save,
debounced 500ms per git repository root. This lets git-ai attribute saved lines
to the human author.

Installation (automatic via `git-ai install-hooks`):
  The plugin is written to:
    macOS:   ~/Library/Application Support/Sublime Text/Packages/git-ai/git_ai.py
    Linux:   ~/.config/sublime-text/Packages/git-ai/git_ai.py
    Windows: %APPDATA%\\Sublime Text\\Packages\\git-ai\\git_ai.py
  Sublime Text hot-reloads Python packages — no restart needed.
"""

from __future__ import annotations

import json
import os
import subprocess
import threading

import sublime
import sublime_plugin

# git-ai binary path (substituted at install time by `git-ai install-hooks`)
GIT_AI_BIN = "__GIT_AI_BINARY_PATH__"

_lock = threading.Lock()
_timers: dict = {}   # repo_root -> threading.Timer
_pending: dict = {}  # repo_root -> dict[path, content]


def _find_repo_root(file_path: str) -> str | None:
    """Walk up from file_path to find the nearest .git directory."""
    d = os.path.dirname(os.path.abspath(file_path))
    while True:
        if os.path.isdir(os.path.join(d, ".git")):
            return d
        parent = os.path.dirname(d)
        if parent == d:
            return None
        d = parent


def _fire_checkpoint(repo_root: str) -> None:
    """Called after the debounce window; sends accumulated files to git-ai."""
    with _lock:
        _timers.pop(repo_root, None)
        files: dict = _pending.pop(repo_root, {})

    if not files:
        return

    payload = json.dumps({
        "editor": "sublime-text",
        "editor_version": sublime.version(),
        "extension_version": "1.0.0",
        "cwd": repo_root,
        "edited_filepaths": list(files.keys()),
        "dirty_files": files,
    })

    try:
        proc = subprocess.Popen(
            [GIT_AI_BIN, "checkpoint", "known_human", "--hook-input", "stdin"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            cwd=repo_root,
        )
        stdout, stderr = proc.communicate(input=payload.encode("utf-8"), timeout=15)
        if proc.returncode != 0:
            print(f"[git-ai] checkpoint exited with code {proc.returncode}: {stderr.decode('utf-8', errors='replace')[:200]}")
    except Exception as exc:
        print(f"[git-ai] checkpoint known_human error: {exc}")


class GitAiKnownHumanListener(sublime_plugin.EventListener):
    """Listens for post-save events and fires the known_human checkpoint."""

    def on_post_save_async(self, view: sublime.View) -> None:
        file_path = view.file_name()
        if not file_path:
            return

        # Skip IDE-internal paths
        norm = file_path.replace("\\", "/")
        if "/.git/" in norm:
            return
        if file_path.endswith(".sublime-workspace") or file_path.endswith(".sublime-project"):
            return

        repo_root = _find_repo_root(file_path)
        if not repo_root:
            return

        # Read current content from the view buffer (already saved)
        content = view.substr(sublime.Region(0, view.size()))

        with _lock:
            if repo_root not in _pending:
                _pending[repo_root] = {}
            _pending[repo_root][file_path] = content

            # Cancel existing debounce timer for this repo root and start a new one
            existing = _timers.get(repo_root)
            if existing is not None:
                existing.cancel()

            timer = threading.Timer(0.5, _fire_checkpoint, args=[repo_root])
            _timers[repo_root] = timer
            timer.start()


def plugin_unloaded():
    with _lock:
        for t in _timers.values():
            t.cancel()
        _timers.clear()
        _pending.clear()
