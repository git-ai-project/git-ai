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

  // Files that have received a genuine human keystroke since their last save.
  // Cleared on every save of that file, regardless of whether we checkpoint.
  private keystrokeDirty = new Set<string>();

  constructor(
    private readonly editorVersion: string,
    private readonly extensionVersion: string,
  ) {}

  public handleContentChangeEvent(event: vscode.TextDocumentChangeEvent): void {
    const doc = event.document;
    if (doc.uri.scheme !== "file") {
      return;
    }
    const filePath = doc.uri.fsPath;
    if (this.isInternalVSCodePath(filePath)) {
      return;
    }
    if (!this.isHumanKeystroke(event)) {
      return;
    }
    this.keystrokeDirty.add(filePath);
  }

  public handleCloseEvent(doc: vscode.TextDocument): void {
    if (doc.uri.scheme !== "file") {
      return;
    }
    this.keystrokeDirty.delete(doc.uri.fsPath);
  }

  private isHumanKeystroke(event: vscode.TextDocumentChangeEvent): boolean {
    // Human typing requires the doc to be the active editor. AI WorkspaceEdit
    // writes typically target non-active documents.
    if (vscode.window.activeTextEditor?.document !== event.document) {
      return false;
    }
    // Exclude undo/redo.
    if (event.reason !== undefined) {
      return false;
    }
    return event.contentChanges.some(
      (c) => c.range.end.line - c.range.start.line <= 1,
    );
  }

  public handleSaveEvent(doc: vscode.TextDocument): void {
    if (doc.uri.scheme !== "file") {
      return;
    }

    const filePath = doc.uri.fsPath;

    if (this.isInternalVSCodePath(filePath)) {
      console.log("[git-ai] KnownHumanCheckpointManager: Ignoring internal VSCode file:", filePath);
      return;
    }

    // Save resets the keystroke-evidence window for this file regardless of
    // whether we end up checkpointing.
    const hadKeystroke = this.keystrokeDirty.delete(filePath);

    const visible = vscode.window.visibleTextEditors.some(
      (e) => e.document.uri.scheme === "file" && e.document.uri.fsPath === filePath,
    );
    if (!visible) {
      console.log("[git-ai] KnownHumanCheckpointManager: File not visible in any editor, skipping:", filePath);
      return;
    }

    if (!hadKeystroke) {
      console.log("[git-ai] KnownHumanCheckpointManager: No human keystroke since last save, skipping:", filePath);
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
    this.keystrokeDirty.clear();
  }
}
