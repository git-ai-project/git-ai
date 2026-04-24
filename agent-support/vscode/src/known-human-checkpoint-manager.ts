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

  // per repo root: absolute file path -> content captured at the time of the
  // save event. We capture eagerly because Cascade can overwrite the buffer
  // during the debounce window and re-reading at fire time would send the
  // wrong content.
  private pendingPaths = new Map<string, Map<string, string>>();

  // Per-file: buffer content at the moment of the most recent Keyboard-kind
  // selection change. Typing produces such selections (cursor moves with each
  // character); AI WorkspaceEdits do not. At save time we require the buffer
  // to still equal this snapshot — if Cascade overwrote the buffer in between,
  // the snapshot won't match and we skip.
  private humanSnapshot = new Map<string, string>();
  // Records that we've seen a Keyboard selection on this file since last save —
  // used purely for noisy logging.
  private sawKbdSinceSave = new Set<string>();

  constructor(
    private readonly editorVersion: string,
    private readonly extensionVersion: string,
  ) {
    console.log("[git-ai][KHC] v6 (snapshot-anchored) constructed");
  }

  public handleSelectionChangeEvent(event: vscode.TextEditorSelectionChangeEvent): void {
    if (event.kind !== vscode.TextEditorSelectionChangeKind.Keyboard) {
      return;
    }
    const doc = event.textEditor.document;
    if (doc.uri.scheme !== "file") {
      return;
    }
    const filePath = doc.uri.fsPath;
    if (this.isInternalVSCodePath(filePath)) {
      return;
    }
    this.humanSnapshot.set(filePath, doc.getText());
    this.sawKbdSinceSave.add(filePath);
    console.log("[git-ai][KHC] keyboard-selection — snapshot updated", filePath, "len=" + doc.getText().length);
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
    console.log("[git-ai][KHC] edit observed", filePath, "changes=" + event.contentChanges.length, "newLen=" + doc.getText().length);
  }

  public handleCloseEvent(doc: vscode.TextDocument): void {
    if (doc.uri.scheme !== "file") {
      return;
    }
    const fsPath = doc.uri.fsPath;
    this.humanSnapshot.delete(fsPath);
    this.sawKbdSinceSave.delete(fsPath);
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

    const visible = vscode.window.visibleTextEditors.some(
      (e) => e.document.uri.scheme === "file" && e.document.uri.fsPath === filePath,
    );
    const snapshot = this.humanSnapshot.get(filePath);
    const currentText = doc.getText();
    const snapshotMatches = snapshot !== undefined && snapshot === currentText;
    const sawKbd = this.sawKbdSinceSave.delete(filePath);

    console.log(
      "[git-ai][KHC] save gates visible=" + visible,
      "sawKbdSinceSave=" + sawKbd,
      "snapshotMatches=" + snapshotMatches,
      "(snapshotLen=" + (snapshot?.length ?? "none") + " currentLen=" + currentText.length + ")",
      "path=" + filePath,
    );

    if (!visible) {
      console.log("[git-ai][KHC] SKIP: not visible —", filePath);
      return;
    }
    if (!snapshotMatches) {
      // Either no snapshot ever taken (no human keyboard activity on this file)
      // or buffer changed since the last keyboard selection (Cascade overwrite).
      // Reset snapshot to the current buffer state so subsequent typing rebuilds
      // a clean baseline.
      this.humanSnapshot.delete(filePath);
      console.log("[git-ai][KHC] SKIP: snapshot mismatch —", filePath);
      return;
    }

    const repoRoot = getGitRepoRoot(doc.uri);
    if (!repoRoot) {
      console.log("[git-ai] KnownHumanCheckpointManager: No git repo found for", filePath, "- skipping");
      return;
    }

    // Capture content NOW (matches the snapshot we just verified). If Cascade
    // overwrites during the debounce window, that overwrite triggers a separate
    // save event which fails the snapshot gate, but our captured content here
    // remains the human-typed buffer.
    let pending = this.pendingPaths.get(repoRoot);
    if (!pending) {
      pending = new Map();
      this.pendingPaths.set(repoRoot, pending);
    }
    pending.set(filePath, currentText);

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
    // Use the content captured at save time, not what's currently in the
    // buffer or on disk — Cascade may have overwritten either during the
    // debounce window.
    const dirtyFiles: Record<string, string> = {};
    for (const [absolutePath, capturedContent] of paths) {
      dirtyFiles[absolutePath] = capturedContent;
    }
    paths.clear();

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
    this.humanSnapshot.clear();
    this.sawKbdSinceSave.clear();
  }
}
