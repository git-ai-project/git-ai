# git-ai Visual Studio Extension: Technical Design

## 1. Overview

This document describes the design of a Visual Studio (VSIX) extension that detects AI-generated code edits (primarily GitHub Copilot) and records them via the `git-ai` CLI. The extension follows the same architectural patterns as the existing IntelliJ plugin.

### Goals

- Detect when GitHub Copilot (inline completions or chat edits) modifies code in Visual Studio
- Record AI-authored edits by calling `git ai checkpoint agent-v1 --hook-input stdin`
- Record human edits as `known_human` checkpoints so git-ai can distinguish the before/after boundary
- Auto-install via `git ai install-hooks`

### Non-goals

- Supporting Visual Studio for Mac (discontinued by Microsoft)
- Supporting Visual Studio versions older than 2022 (17.0)

---

## 2. Background: How the existing extensions work

### 2.1 IntelliJ plugin (our primary reference)

The IntelliJ plugin uses **stack trace analysis** to detect AI edits. Every text change in IntelliJ fires a `DocumentEvent` on the EDT (Event Dispatch Thread). The plugin captures `Thread.currentThread().stackTrace` and inspects it for known AI agent class prefixes:

```
com.github.copilot.*         -> "github-copilot-jetbrains"
com.intellij.ml.llm.matterhorn.* -> "junie"
```

If a HIGH-confidence match is found, the edit is recorded as an AI checkpoint. The plugin has three listeners:

| Listener | Trigger | Checkpoint type |
|---|---|---|
| `DocumentChangeListener` | `beforeDocumentChange` / `documentChanged` | `agent-v1` (AI) with before/after pair |
| `VfsRefreshListener` | Disk writes from external processes | `agent-v1` (AI) sweep checkpoint |
| `DocumentSaveListener` | User-initiated saves | `known_human` |

### 2.2 VS Code extension

The VS Code extension uses a completely different strategy: **URI scheme sniffing**. When Copilot's chat feature edits a file, VS Code opens a temporary document with the URI scheme `chat-editing-snapshot-text-model://`. The extension watches for these URIs to detect AI edits.

This mechanism is VS Code-specific and does not exist in Visual Studio or IntelliJ.

### 2.3 Why Visual Studio is closer to IntelliJ

| Capability | VS Code | IntelliJ | Visual Studio |
|---|---|---|---|
| Text change events | `onDidChangeTextDocument` | `BulkAwareDocumentListener` | `ITextBuffer.Changed` |
| Thread model | Multi-process (extension host) | Single EDT | Single UI thread |
| Stack trace visibility | Not useful (cross-process) | Full call chain visible | Full call chain visible |
| AI edit URI tagging | Yes (`chat-editing-snapshot-text-model://`) | No | No |
| Extension language | TypeScript | Kotlin/JVM | C#/.NET |

Because Visual Studio runs Copilot extensions in-process on the UI thread (like IntelliJ), stack trace analysis is the correct detection strategy.

---

## 3. Architecture

### 3.1 Component diagram

```
┌─────────────────────────────────────────────────────────┐
│                    Visual Studio                         │
│                                                         │
│  ┌──────────────────────┐                               │
│  │   GitAiPackage       │  (AsyncPackage entry point)   │
│  │   ├── BinaryResolver │  Locate git-ai binary         │
│  │   └── Registers:     │                               │
│  │       ├── TextBufferListener   ──┐                   │
│  │       └── DocumentSaveListener ──┤                   │
│  └──────────────────────┘           │                   │
│                                     ▼                   │
│  ┌──────────────────────────────────────────────┐       │
│  │  CopilotEditDetector                         │       │
│  │  Inspects Environment.StackTrace for:        │       │
│  │   • GitHub.Copilot.*                         │       │
│  │   • Microsoft.VisualStudio.Copilot.*         │       │
│  │   • Microsoft.VisualStudio.Editor.           │       │
│  │     Implementation.Copilot.*                 │       │
│  │   • Microsoft.VisualStudio.Conversations.    │       │
│  │     UI.Internal.Copilot.*                    │       │
│  └──────────────────────────────────────────────┘       │
│                    │                                     │
│                    ▼                                     │
│  ┌──────────────────────────────────────────────┐       │
│  │  CheckpointService                           │       │
│  │  Spawns: git-ai checkpoint agent-v1          │       │
│  │          --hook-input stdin                   │       │
│  │  Writes JSON to stdin, reads exit code       │       │
│  └──────────────────────────────────────────────┘       │
│                    │                                     │
└────────────────────│─────────────────────────────────────┘
                     ▼
              ┌─────────────┐
              │  git-ai CLI │
              │  (Rust)     │
              └──────┬──────┘
                     ▼
              ┌─────────────┐
              │ Git Notes   │
              │ refs/notes/ │
              │    ai       │
              └─────────────┘
```

### 3.2 Event flow

```
User accepts Copilot suggestion (Tab) or Copilot chat applies edit
    │
    ▼
ITextBuffer.Changed fires on UI thread
    │
    ▼
TextBufferListener captures new StackTrace()
    │
    ▼
CopilotEditDetector.Analyze(stackTrace)
    │
    ├── HIGH confidence match (Copilot namespace prefix found)
    │   │
    │   ├── 1. Send "human" before_edit checkpoint (pre-edit content via e.Before)
    │   │      { "type": "human", "repo_working_dir": "...", "will_edit_filepaths": [...], "dirty_files": {...} }
    │   │
    │   └── 2. Debounce 300ms, then send "ai_agent" after_edit checkpoint
    │          { "type": "ai_agent", "repo_working_dir": "...", "edited_filepaths": [...],
    │            "agent_name": "github-copilot-visualstudio", "model": "unknown",
    │            "conversation_id": "<session_id>", "dirty_files": {...} }
    │
    └── No match or MEDIUM confidence (human edit)
        │
        └── On save: send known_human checkpoint (debounced 500ms)
            { "editor": "visualstudio", "editor_version": "17.x", "extension_version": "0.1.0",
              "cwd": "...", "edited_filepaths": [...], "dirty_files": {...} }
```

---

## 4. Detailed component design

### 4.1 GitAiPackage (entry point)

**File**: `src/GitAiVS/GitAiPackage.cs`

The `AsyncPackage` subclass that Visual Studio loads on startup. Responsibilities:

- Resolve the git-ai binary path (via `BinaryResolver`)
- Set `CheckpointService.Current` for MEF-exported components to access
- Subscribe to `IVsRunningDocTableEvents` (for `DocumentSaveListener`)
- Display info bar if git-ai is not installed (currently logs only, UI planned)

Auto-load contexts: `NoSolution`, `SolutionExists`, `SolutionHasMultipleProjects`, `SolutionHasSingleProject` -- ensures the package loads regardless of how VS starts.

### 4.2 BinaryResolver

**File**: `src/GitAiVS/Services/BinaryResolver.cs`

Locates the `git-ai` (or `git-ai.exe`) binary. Search order:

1. `%USERPROFILE%\.git-ai\bin\git-ai.exe` (standard install location on Windows)
2. `%USERPROFILE%\.git-ai-local-dev\gitwrap\bin\git-ai.exe` (nix dev path)
3. `PATH` lookup via `where git-ai` (Windows) or `which git-ai` (Mac/Linux)

After finding the binary, runs `git-ai version` to verify it meets the minimum version requirement (currently `1.0.23`, matching IntelliJ). Caches the resolved path for the session. Logs structured error messages when the binary is not found or version is too old, matching IntelliJ's `GitAiService.findGitAiBinary()` pattern.

### 4.3 GitRepoResolver

**File**: `src/GitAiVS/Services/GitRepoResolver.cs`

Finds the git repository root for a given file path by walking up the directory tree looking for a `.git` directory (supports both regular repos and worktrees where `.git` is a file). `ToRelativePath` uses case-insensitive comparison on Windows.

### 4.4 CopilotEditDetector (stack trace analysis)

**File**: `src/GitAiVS/Detection/CopilotEditDetector.cs`

The core detection logic, directly modeled after IntelliJ's `StackTraceAnalyzer.kt`.

**How it works**: When `ITextBuffer.Changed` fires, the calling thread's stack trace contains frames from whatever code triggered the change. If Copilot triggered it, frames from Copilot's assemblies will be present.

**Discovered namespace prefixes** (empirically verified on VS 2022/2025):

| Agent name | Namespace prefixes (HIGH confidence) | Class keywords (MEDIUM confidence) |
|---|---|---|
| `github-copilot-visualstudio` | `GitHub.Copilot`, `Microsoft.VisualStudio.Copilot`, `Microsoft.VisualStudio.Conversations.UI.Internal.Copilot` | `copilot` |

**Specific stack frames observed**:
- Chat edits: `Microsoft.VisualStudio.Conversations.UI.Internal.CopilotBufferUpdater.ApplyEditsAndSaveAsync`

**Not detectable via stack trace**:
- Inline completions (Tab accept): Goes through `Microsoft.VisualStudio.Editor.Implementation.SuggestionService.AcceptSuggestionCommandHandler`, which is generic VS infrastructure for all inline suggestion providers, not Copilot-specific. `CopilotPreemptingCommandFilter.Exec` is also present but fires for ALL keystrokes (human and AI) so cannot be used.

**Confidence levels**:

- **HIGH**: A stack frame's full class name starts with a known namespace prefix. Only HIGH-confidence matches trigger checkpoints.
- **MEDIUM**: A stack frame's class name contains a known keyword. Logged for debugging but does not trigger checkpoints.
- **NONE**: No AI agent patterns detected.

### 4.5 TextBufferListener

**File**: `src/GitAiVS/Listeners/TextBufferListener.cs`

Attaches to every opened text editor via MEF `[Export(typeof(IVsTextViewCreationListener))]` and listens for `ITextBuffer.Changed` events.

**On each change event**:

1. Capture `new StackTrace()` on the calling thread
2. Pass to `CopilotEditDetector.Analyze()`
3. If HIGH confidence AI edit:
   a. If no recent `before_edit` was sent for this file (5s expiry), send a human checkpoint with pre-edit content from `e.Before.GetText()`
   b. Cancel any pending debounce timer for this file
   c. Schedule a new 300ms debounce timer; when it fires, send an `ai_agent` after_edit checkpoint with `buffer.CurrentSnapshot.GetText()`

**Before_edit timing**: Unlike our initial implementation which incorrectly used post-change content, we now use `TextContentChangedEventArgs.Before` which provides the pre-edit snapshot. This matches IntelliJ's `beforeDocumentChange` pattern.

### 4.6 DocumentSaveListener

**File**: `src/GitAiVS/Listeners/DocumentSaveListener.cs`

Listens for file save events via `IVsRunningDocTableEvents.OnAfterSave` and sends `known_human` checkpoints. Modeled after IntelliJ's `DocumentSaveListener.kt`.

Uses `[SAVE]` log prefix for grep-ability. Debounces 500ms per workspace root. Filters `.vs/` internal paths.

### 4.7 CheckpointService

**File**: `src/GitAiVS/Services/CheckpointService.cs`

Spawns the `git-ai` CLI process and writes JSON to stdin. Modeled after IntelliJ's `GitAiService.checkpoint()`:

- Reads both stdout and stderr separately
- Logs structured multi-line error blocks on failure (Command, Exit code, Stdout, Stderr)
- 30s timeout for checkpoint commands
- Never throws -- returns `bool` for fire-and-forget safety

Uses a static `Current` singleton pattern so MEF-created `TextBufferListener` can access it without manual wiring.

### 4.8 JSON models (checkpoint input schemas)

**File**: `src/GitAiVS/Models/CheckpointInput.cs`

These match the Rust `AgentV1Payload` enum and `KnownHumanPreset` exactly. Uses `System.Text.Json` with `[JsonPropertyName]` attributes for snake_case serialization.

---

## 5. CLI installer (Rust side)

### 5.1 VisualStudioInstaller

**File**: `src/mdm/agents/visual_studio.rs`

A `HookInstaller` implementation that auto-detects Visual Studio installations using `vswhere.exe` and checks for the VSIX extension.

**Status**: Detection and check logic implemented. VSIX auto-install is stubbed (falls back to marketplace URL). Full auto-install depends on marketplace publishing.

### 5.2 Platform scope

The `VisualStudioInstaller` is **Windows-only**. Returns `tool_installed: false` on non-Windows platforms.

---

## 6. Project structure

```
agent-support/visualstudio/
├── DESIGN.md                          # This document
├── README.md                          # Build/debug/test instructions
├── GitAiVS.sln                        # Solution file
└── src/
    ├── GitAiVS/
    │   ├── GitAiVS.csproj             # Project file (targets net48, VS 2022+)
    │   ├── source.extension.vsixmanifest
    │   ├── LICENSE
    │   ├── GitAiPackage.cs
    │   ├── Services/
    │   │   ├── BinaryResolver.cs
    │   │   ├── GitRepoResolver.cs
    │   │   └── CheckpointService.cs
    │   ├── Detection/
    │   │   └── CopilotEditDetector.cs
    │   ├── Listeners/
    │   │   ├── TextBufferListener.cs
    │   │   └── DocumentSaveListener.cs
    │   └── Models/
    │       └── CheckpointInput.cs
    └── GitAiVS.Tests/
        ├── GitAiVS.Tests.csproj
        ├── BinaryResolverTests.cs
        ├── GitRepoResolverTests.cs
        ├── CheckpointInputTests.cs
        └── DocumentSaveListenerTests.cs
```

---

## 7. Testing strategy

### 7.1 Unit tests

Pure function tests that don't require a VS host (following IntelliJ's `VfsRefreshListenerTest.kt` pattern):

- `BinaryResolverTests`: `ParseVersion` with valid/invalid/prerelease/garbage strings
- `GitRepoResolverTests`: `ToRelativePath` with case variants, path separators, non-matching prefixes
- `CheckpointInputTests`: JSON serialization schema compliance, null field omission
- `DocumentSaveListenerTests`: `IsInternalPath` filtering logic

### 7.2 Integration tests (manual)

- Install the extension in VS 2022 with Copilot enabled
- Accept inline suggestions and verify `agent-v1` checkpoints are created
- Use Copilot chat to edit files and verify checkpoints
- Type manually and verify `known_human` checkpoints on save
- Run `git ai status` to confirm attribution is recorded
- Run `git ai log` after committing to verify notes are attached

---

## 8. Known limitations and future work

### Implemented in v0.1.0

- Stack trace detection for GitHub Copilot (inline + chat)
- Human before_edit / AI after_edit checkpoint pairs
- known_human checkpoints on save
- Rust `VisualStudioInstaller` for detection

### Planned for future versions

- **VfsRefreshListener equivalent**: A `FileSystemWatcher`-based listener to catch disk-based AI edits (e.g., agents that apply patches via file writes). IntelliJ has this; VS does not yet.
- **TabCompletionFilter**: An `IOleCommandTarget` that intercepts Tab key presses as a supplementary signal for inline completion detection. Was prototyped but removed from v0.1.0 as stack trace analysis proved sufficient. Can be re-added if edge cases require it.
- **Info bar notification**: `IVsInfoBarUIFactory` to show a visible UI notification when git-ai is not installed.
- **Telemetry**: PostHog + Sentry integration, matching IntelliJ's `TelemetryService`.
- **VSIX auto-install**: Full `install_vsix()` implementation in the Rust installer.
- **Additional AI agents**: Extend `KnownAgents` in `CopilotEditDetector` for future VS AI tools.
