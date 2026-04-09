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

  constructor(
    private readonly editorVersion: string,
    private readonly extensionVersion: string,
  ) {}

  public handleSaveEvent(doc: vscode.TextDocument): void {
    if (doc.uri.scheme !== "file") {
      return;
    }

    const filePath = doc.uri.fsPath;

    if (this.isInternalVSCodePath(filePath)) {
      console.log("[git-ai] KnownHumanCheckpointManager: Ignoring internal VSCode file:", filePath);
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
      this.executeCheckpoint(repoRoot);
    }, this.debounceMs);

    this.pendingTimers.set(repoRoot, timer);
    console.log("[git-ai] KnownHumanCheckpointManager: Save queued for", filePath);
  }

  private executeCheckpoint(repoRoot: string): void {
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
      // Use open document buffer if available (handles codespaces/remote lag);
      // fall back to reading from the saved document directly.
      const content = doc ? doc.getText() : null;
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
  }
}
