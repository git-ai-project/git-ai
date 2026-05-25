# Junie support

Junie CLI currently exposes a `SessionStart` hook, but not pre-edit or post-edit file hooks. The `junie` preset uses that hook to create a human baseline checkpoint for files that are already dirty when a Junie session starts or resumes.

Add this hook to `~/.junie/config.json`:

```json
{
  "hooks": {
    "SessionStart": [
      {
        "type": "command",
        "command": "git-ai checkpoint junie --hook-input stdin"
      }
    ]
  }
}
```

If `git-ai` is not on your `PATH`, use the absolute path to the binary in the command.

Junie sends hook input on stdin, for example:

```json
{"hook_event_name":"SessionStart","source":"startup"}
```

Because Junie does not yet expose file edit lifecycle hooks, this preset only establishes the pre-session baseline. It is designed to be extended when Junie adds richer hook events.
