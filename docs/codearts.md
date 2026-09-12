# CodeArts Agent support

Git AI supports Huawei Cloud CodeArts Agent (华为云码道代码智能体) through its JavaScript/TypeScript tool hooks. The integration uses the same hook lifecycle as OpenCode and records the agent as `codearts`.

## Install

1. Install and start a CodeArts client with hook support: CodeArts Agent IDE, its editor extension, or CodeArts CLI.
2. Install Git AI, then run:

   ```sh
   git-ai install-hooks
   ```

3. Restart the CodeArts IDE, reload the editor hosting the extension, or start a new CodeArts CLI process to load the plugin.

The installer detects the `codearts` command or a `.codeartsdoer` directory in your home directory or current project. It writes the plugin to `~/.codeartsdoer/plugins/git-ai.ts`, which applies to all projects for the current user. On Windows, `~` is `%USERPROFILE%`.

The plugin contains the absolute path to the installed Git AI binary. Running `git-ai install-hooks` again updates it when necessary. You can preview changes with `git-ai install-hooks --dry-run`.

## Attribution

Before a tracked file edit, the plugin captures any existing changes as untracked. After the edit, it attributes the new changes to CodeArts and its session. It also records matching shell tool calls using Git AI's existing shell checkpoint flow. Commit normally; Git AI's trace2 daemon attaches attribution to the commit.

The plugin captures the model identifier from CodeArts chat hooks when available. If CodeArts does not provide model information for the session, the model is recorded as `unknown`. Set `GIT_AI_CODEARTS_DEBUG=1` or `GIT_AI_DEBUG=1` before launching CodeArts to log checkpoint failures. Hook failures do not interrupt the agent's tool execution.

File checkpoints use the same paths before and after an edit. Paths first
reported in post-tool metadata are not added to the attribution scope, because
their previous untracked changes cannot safely be distinguished from AI edits.
Edits without any resolvable pre-tool paths are skipped. Failed or cancelled
calls are cleared when the client reports a tool error; retained pending calls
are also capped at 1,024 if terminal events are missing. An evicted call skips
its post-checkpoint.

After a tracked tool call and when an assistant message finishes, Git AI reads
that session's conversation from the CodeArts SQLite database through its
existing asynchronous transcript worker. The completion notification collects
final replies and tool results written after the tool hook, without creating
another code-attribution checkpoint. Completed conversations without editing
tools are also collected when the client is running in a Git repository.
User messages, assistant messages and tool calls retain their native data,
with the agent recorded as `codearts`. Child sessions retain their parent
session IDs when CodeArts provides them. Transcript collection follows Git
AI's existing configuration and secret-redaction rules.

The plugin resolves the database path inside the CodeArts process, using
`KERNEL_DATA_DIR`, or `XDG_DATA_HOME` and `SCENARIO` with the kernel's defaults.
The standard database is `opencode.db`; the CLI normally stores it under
`~/.codeartsdoer/cli-data`. `OPENCODE_DB` can select another absolute path or a
path relative to the kernel's data directory. An in-memory database
(`OPENCODE_DB=:memory:`) cannot be collected by Git AI. Missing database files
do not prevent code attribution. This integration uses the SQLite schema
verified in CodeArts CLI 26.8.1 and VS Code AgentKernel 26.8.101; custom build
channels with a different database name should set `OPENCODE_DB` explicitly.

Legacy CodeArts Snap inline completions are not covered by these hooks.

### Tool coverage

The CodeArts installer embeds the same plugin template as OpenCode. Its tracked
tool-name set includes every editing and shell tool recognized by this
repository's OpenCode plugin, plus CodeArts `deleteFile`:

| Tool names | Handling | Verification in this implementation |
| --- | --- | --- |
| `edit`, `write` | Capture changes to the supplied file paths | Real CodeArts CLI 26.8.1 / MiMo edits, commits, stats and blame verified |
| `bash`, `shell` | Reuse OpenCode's shell snapshot attribution | Real `bash` edits verified; `shell` alias covered by parser tests |
| `multiedit` | Extract and deduplicate paths across nested edits | Parser and `TestRepo` commit/blame tests, including unrelated dirty files; no real CodeArts run yet |
| `apply_patch` | Extract add, update, delete and move paths from `*** ... File:` / `*** Move to:` headers | Parser and `TestRepo` add/move/delete commit tests; no real CodeArts run yet |
| `patch`, `applypatch` | Recognize the aliases and use the same path extractor | Implemented; no separate real run yet |
| `deleteFile` | Capture the explicitly named deletion target | Plugin-hook and parser tests; no real deletion attribution run yet |

These are names the integration accepts, not a promise that every CodeArts
version or model exposes every tool. File tools must provide paths through
supported fields such as `filePath`, `file_path`, `path`, `files`, nested edit
objects, or the supported patch headers. Recognizing `patch` does not imply
support for every patch format; a raw unified diff without supported path
fields or headers is not currently parsed.

The plugin decodes standard `file://` paths, including escaped spaces and
Windows drive letters. On Windows, `edit` and `deleteFile` also follow CodeArts
26.8.1's `/d/path` to `d:/path` conversion. That conversion is not applied to
other tools, whose execution uses different path handling.

Read-only tools are skipped. Arbitrarily named custom or MCP file-writing tools
are not automatically tracked. Subagent edits depend on receiving their own
tracked tool hooks. Parent/child conversation links are covered by integration
tests; real subagent editing has not yet been verified.
The shared shell snapshot algorithm detects created and modified files, not
pure file deletions. Shell deletion attribution is therefore not a guaranteed
capability of either integration.

To remove only this integration, delete `~/.codeartsdoer/plugins/git-ai.ts` and restart CodeArts. The `git-ai uninstall-hooks` command removes Git AI hooks from all detected agents, including CodeArts.

## Development

The CodeArts installer generates its plugin from `agent-support/opencode/git-ai.ts`. Shared tool handling fixes therefore apply to both clients. Use the repository's standard development commands:

```sh
task test TEST_FILTER=codearts
task lint
task fmt
task dev
```

For a real non-interactive CLI check, select an available model explicitly and
send the prompt through stdin. For example, with MiMo configured:

```powershell
codearts models
Get-Content -Raw .\prompt.txt | codearts run --format json --auto --model mimo/mimo-v2.5
```

This follows the invocation guidance in [Multica PR #6985](https://github.com/multica-ai/multica/pull/6985).
Use a model listed by your installation. CodeArts CLI 26.8.1 also checks for
`CODEARTS_CLI_AK` and `CODEARTS_CLI_SK` in the process environment, including when
an external model provider is selected; configure authentication according to
your provider and CodeArts setup.

`--auto` controls shell execution. It does not approve native file edits:
CodeArts automatically rejects permissions configured as `ask` in
non-interactive `run` mode. Use the normal one-time approval flow in an
interactive client when testing native `edit` or `write` calls. See the
[CodeArts permission documentation](https://support.huaweicloud.com/usermanual-cli/codeartsagent_cli_0006.html).

After CodeArts has actually modified files, run the project's checks, commit
the test changes, and wait for asynchronous attribution before inspecting it:

```sh
git add <test-files>
git commit -m "Verify CodeArts attribution"
git-ai await --timeout 30
git-ai stats HEAD --json
git-ai blame <test-file> --json
git notes --ref=ai show HEAD
```

Check the tool result and file contents as well as the process exit code.
Confirm that the authorship note records `codearts`, the selected model, and
the actual CodeArts session ID. Shell-generated changes and native file-tool
changes should be verified separately so that shell fallback cannot mask a
failed native edit.

## Huawei Cloud references

- [CodeArts Agent IDE and extension hooks](https://support.huaweicloud.com/intl/zh-cn/usermanual-codeartsagent/codeartsagent_ug_0036.html): plugin paths, reload requirements, and hook event signatures.
- [CodeArts CLI hooks](https://support.huaweicloud.com/usermanual-cli/codeartsagent_cli_0018.html): shared plugin paths and lifecycle events.
- [CodeArts CLI agents and tools](https://support.huaweicloud.com/intl/zh-cn/usermanual-cli/codeartsagent_cli_0031.html): built-in file and shell tools.
