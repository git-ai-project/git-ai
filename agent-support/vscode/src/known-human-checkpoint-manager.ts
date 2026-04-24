import * as vscode from "vscode";
import * as path from "node:path";
import { spawn } from "child_process";
import { getGitAiBinary } from "./utils/binary-path";
import { getGitRepoRoot } from "./utils/git-api";

/**
 * Fires a `git-ai checkpoint known_human --hook-input stdin` whenever a
 * document is saved. Debounces per repo root over a 500ms window so that
 * bulk saves (e.g. "Save All") are batched into one checkpoint call.
 *
 * Skips non-file-scheme documents and .vscode/ internal files.
 */
export class KnownHumanCheckpointManager {
  private readonly debounceMs = 500;

  // per repo root: pending debounce timer
  private pendingTimers = new Map<string, NodeJS.Timeout>();

  // per repo root: set of absolute file paths queued in current debounce window
  private pendingPaths = new Map<string, Set<string>>();

  // Per-file timestamps. Comparing at save time is order-independent — typing
  // fires contentChange before selectionChange, so checking at change time
  // would miss the first keystroke after a save.
  private lastContentChange = new Map<string, number>();      // any contentChange
  private lastKeyboardSelection = new Map<string, number>();  // Keyboard-kind only
  private lastSave = new Map<string, number>();

  constructor(
    private readonly editorVersion: string,
    private readonly extensionVersion: string,
  ) {
    console.log("[git-ai][KHC] v5 constructed");
  }

  public handleSelectionChangeEvent(event: vscode.TextEditorSelectionChangeEvent): void {
    if (event.kind !== vscode.TextEditorSelectionChangeKind.Keyboard) {
      return;
    }
    const doc = event.textEditor.document;
    if (doc.uri.scheme !== "file") {
      return;
    }
    this.lastKeyboardSelection.set(doc.uri.fsPath, Date.now());
    console.log("[git-ai][KHC] keyboard-selection", doc.uri.fsPath);
  }

  public handleContentChangeEvent(event: vscode.TextDocumentChangeEvent): void {
    const doc = event.document;
    if (doc.uri.scheme !== "file") {
      return;
    }
    const filePath = doc.uri.fsPath;
    if (this.isInternalVSCodePath(filePath)) {
      return;
    }
    if (event.contentChanges.length === 0) {
      return;
    }
    this.lastContentChange.set(filePath, Date.now());
    console.log("[git-ai][KHC] edit recorded", filePath, "changes=" + event.contentChanges.length);
  }

  public handleCloseEvent(doc: vscode.TextDocument): void {
    if (doc.uri.scheme !== "file") {
      return;
    }
    const fsPath = doc.uri.fsPath;
    this.lastContentChange.delete(fsPath);
    this.lastKeyboardSelection.delete(fsPath);
    this.lastSave.delete(fsPath);
  }

  public handleSaveEvent(doc: vscode.TextDocument): void {
    const filePath = doc.uri.fsPath;
    console.log("[git-ai][KHC] save event scheme=" + doc.uri.scheme, "path=" + filePath);
    if (doc.uri.scheme !== "file") {
      return;
    }

    if (this.isInternalVSCodePath(filePath)) {
      console.log("[git-ai][KHC] Ignoring internal VSCode file:", filePath);
      return;
    }

    const prevSave = this.lastSave.get(filePath) ?? 0;
    const lastChange = this.lastContentChange.get(filePath) ?? 0;
    const lastKbd = this.lastKeyboardSelection.get(filePath) ?? 0;
    this.lastSave.set(filePath, Date.now());

    const visible = vscode.window.visibleTextEditors.some(
      (e) => e.document.uri.scheme === "file" && e.document.uri.fsPath === filePath,
    );
    const changedSinceSave = lastChange > prevSave;
    const kbdSinceSave = lastKbd > prevSave;
    console.log(
      "[git-ai][KHC] save gates visible=" + visible,
      "changedSinceSave=" + changedSinceSave,
      "kbdSinceSave=" + kbdSinceSave,
      "(prevSave=" + prevSave + " lastChange=" + lastChange + " lastKbd=" + lastKbd + ")",
      "path=" + filePath,
    );

    if (!visible) {
      console.log("[git-ai][KHC] SKIP: not visible —", filePath);
      return;
    }
    if (!changedSinceSave) {
      console.log("[git-ai][KHC] SKIP: no edit since last save —", filePath);
      return;
    }
    if (!kbdSinceSave) {
      console.log("[git-ai][KHC] SKIP: no keyboard activity since last save —", filePath);
      return;
    }

    const repoRoot = getGitRepoRoot(doc.uri);
    if (!repoRoot) {
      console.log("[git-ai] KnownHumanCheckpointManager: No git repo found for", filePath, "- skipping");
      return;
    }

    // Accumulate file into pending set for this repo root
    let pending = this.pendingPaths.get(repoRoot);
    if (!pending) {
      pending = new Set();
      this.pendingPaths.set(repoRoot, pending);
    }
    pending.add(filePath);

    // Reset debounce timer
    const existing = this.pendingTimers.get(repoRoot);
    if (existing) {
      clearTimeout(existing);
    }

    const timer = setTimeout(() => {
      this.executeCheckpoint(repoRoot).catch((err) =>
        console.error("[git-ai] KnownHumanCheckpointManager: Checkpoint error:", err)
      );
    }, this.debounceMs);

    this.pendingTimers.set(repoRoot, timer);
    console.log("[git-ai] KnownHumanCheckpointManager: Save queued for", filePath);
  }

  private async executeCheckpoint(repoRoot: string): Promise<void> {
    this.pendingTimers.delete(repoRoot);

    const paths = this.pendingPaths.get(repoRoot);
    if (!paths || paths.size === 0) {
      return;
    }
    const snapshot = [...paths];
    paths.clear();

    // Build dirty_files as absolute path → current content
    const dirtyFiles: Record<string, string> = {};
    for (const absolutePath of snapshot) {
      const doc = vscode.workspace.textDocuments.find(
        (d) => d.uri.fsPath === absolutePath && d.uri.scheme === "file"
      );

      let content: string | null = null;
      if (doc) {
        // Use open document buffer if available (handles codespaces/remote lag)
        content = doc.getText();
      } else {
        // Fall back to reading from disk if document was closed within debounce window
        try {
          const bytes = await vscode.workspace.fs.readFile(vscode.Uri.file(absolutePath));
          content = Buffer.from(bytes).toString("utf-8");
        } catch (err) {
          console.error("[git-ai] KnownHumanCheckpointManager: Failed to read file", absolutePath, err);
        }
      }

      if (content !== null) {
        dirtyFiles[absolutePath] = content;
      }
    }

    if (Object.keys(dirtyFiles).length === 0) {
      return;
    }

    const editedFilepaths = Object.keys(dirtyFiles);

    const hookInput = JSON.stringify({
      editor: "vscode",
      editor_version: this.editorVersion,
      extension_version: this.extensionVersion,
      cwd: repoRoot,
      edited_filepaths: editedFilepaths,
      dirty_files: dirtyFiles,
    });

    console.log("[git-ai] KnownHumanCheckpointManager: Firing known_human checkpoint for", editedFilepaths);

    const proc = spawn(getGitAiBinary(), ["checkpoint", "known_human", "--hook-input", "stdin"], {
      cwd: repoRoot,
    });

    let stdout = "";
    let stderr = "";

    proc.stdout.on("data", (data) => { stdout += data.toString(); });
    proc.stderr.on("data", (data) => { stderr += data.toString(); });

    proc.on("error", (err) => {
      console.error("[git-ai] KnownHumanCheckpointManager: Spawn error:", err.message);
    });

    proc.on("close", (code) => {
      if (code !== 0) {
        console.error("[git-ai] KnownHumanCheckpointManager: Checkpoint exited with code", code, stdout, stderr);
      } else {
        console.log("[git-ai] KnownHumanCheckpointManager: Checkpoint succeeded", stdout.trim());
      }
    });

    proc.stdin.write(hookInput);
    proc.stdin.end();
  }

  private isInternalVSCodePath(filePath: string): boolean {
    const normalized = filePath.replace(/\\/g, "/");
    return normalized.includes("/.vscode/");
  }

  public dispose(): void {
    for (const timer of this.pendingTimers.values()) {
      clearTimeout(timer);
    }
    this.pendingTimers.clear();
    this.pendingPaths.clear();
    this.lastContentChange.clear();
    this.lastKeyboardSelection.clear();
    this.lastSave.clear();
  }
}
