import * as vscode from "vscode";
import * as path from "path";
import * as fs from "fs";
import { exec, spawn } from "child_process";
import { isVersionSatisfied } from "./utils/semver";
import { getGitAiBinary } from "./utils/binary-path";
import { MIN_GIT_AI_VERSION, GIT_AI_INSTALL_DOCS_URL } from "./consts";
import { getGitRepoRoot } from "./utils/git-api";
import { shouldSkipLegacyCopilotHooks } from "./utils/vscode-hooks";

export class AIEditManager {
  private workspaceBaseStoragePath: string | null = null;
  private gitAiVersion: string | null = null;
  private hasShownGitAiErrorMessage = false;
  private readonly legacyCopilotHooksEnabled: boolean;
  private lastHumanCheckpointAt = new Map<string, number>();
  private pendingSaves = new Map<string, {
    timestamp: number;
    timer: NodeJS.Timeout;
  }>();
  private snapshotOpenEvents = new Map<string, {
    timestamp: number;
    count: number;
    uri: vscode.Uri;
  }>();
  private readonly SAVE_EVENT_DEBOUNCE_WINDOW_MS = 300;
  private readonly HUMAN_CHECKPOINT_DEBOUNCE_MS = 500;
  private readonly HUMAN_CHECKPOINT_CLEANUP_INTERVAL_MS = 60000; // 1 minute
  private readonly MAX_SNAPSHOT_AGE_MS = 10_000; // 10 seconds; used to avoid triggering AI checkpoints on stale snapshots
  private cleanupTimer: NodeJS.Timeout;
  private stableFileContent = new Map<string, string>();
  private stableContentTimers = new Map<string, NodeJS.Timeout>();
  private readonly STABLE_CONTENT_DEBOUNCE_MS = 2000;

  constructor(context: vscode.ExtensionContext) {
    this.legacyCopilotHooksEnabled = !shouldSkipLegacyCopilotHooks(vscode.version);
    if (!this.legacyCopilotHooksEnabled) {
      console.log(`[git-ai] AIEditManager: VS Code ${vscode.version} has native hooks; skipping legacy extension checkpoints`);
    }

    if (context.storageUri?.fsPath) {
      this.workspaceBaseStoragePath = path.dirname(context.storageUri.fsPath);
    } else {
      // No workspace active (extension will be re-activated when a workspace is opened)
      console.warn('[git-ai] No workspace storage URI available');
    }

    // Periodically clean up old entries from lastHumanCheckpointAt to avoid memory leaks
    this.cleanupTimer = setInterval(() => {
      this.cleanupOldCheckpointEntries();
    }, this.HUMAN_CHECKPOINT_CLEANUP_INTERVAL_MS);
  }

  public dispose(): void {
    if (this.cleanupTimer) {
      clearInterval(this.cleanupTimer);
    }
    for (const timer of this.stableContentTimers.values()) {
      clearTimeout(timer);
    }
    this.stableContentTimers.clear();
  }

  public areLegacyCopilotHooksEnabled(): boolean {
    return this.legacyCopilotHooksEnabled;
  }

  private cleanupOldCheckpointEntries(): void {
    const now = Date.now();
    const entriesToDelete: string[] = [];

    // Remove entries older than 5 minutes
    const MAX_AGE_MS = 5 * 60 * 1000;

    this.lastHumanCheckpointAt.forEach((timestamp, filePath) => {
      if (now - timestamp > MAX_AGE_MS) {
        entriesToDelete.push(filePath);
      }
    });

    entriesToDelete.forEach(filePath => {
      this.lastHumanCheckpointAt.delete(filePath);
    });

    if (entriesToDelete.length > 0) {
      console.log('[git-ai] AIEditManager: Cleaned up', entriesToDelete.length, 'old checkpoint entries');
    }
  }

  public handleSaveEvent(doc: vscode.TextDocument): void {
    const filePath = doc.uri.fsPath;

    // Clear any existing timer for this file
    const existing = this.pendingSaves.get(filePath);
    if (existing) {
      clearTimeout(existing.timer);
    }

    // Set up new debounce timer
    const timer = setTimeout(() => {
      this.evaluateSaveForCheckpoint(filePath);
    }, this.SAVE_EVENT_DEBOUNCE_WINDOW_MS);

    this.pendingSaves.set(filePath, {
      timestamp: Date.now(),
      timer
    });

    console.log('[git-ai] AIEditManager: Save event tracked for', filePath);
  }

  public handleContentChangeEvent(event: vscode.TextDocumentChangeEvent): void {
    const doc = event.document;
    if (doc.uri.scheme !== "file") {
      return;
    }

    const filePath = doc.uri.fsPath;

    // Clear any existing debounce timer for this file
    const existingTimer = this.stableContentTimers.get(filePath);
    if (existingTimer) {
      clearTimeout(existingTimer);
    }

    // After a quiet period, update the stable content cache
    const timer = setTimeout(() => {
      this.stableFileContent.set(filePath, doc.getText());
      this.stableContentTimers.delete(filePath);
    }, this.STABLE_CONTENT_DEBOUNCE_MS);

    this.stableContentTimers.set(filePath, timer);
  }

  public handleOpenEvent(doc: vscode.TextDocument): void {
    console.log('[git-ai] AIEditManager: Open event detected for', doc);

    // Initialize stable content cache for file:// documents when first opened
    if (doc.uri.scheme === "file" && !this.stableFileContent.has(doc.uri.fsPath)) {
      this.stableFileContent.set(doc.uri.fsPath, doc.getText());
    }

    if (doc.uri.scheme === "chat-editing-snapshot-text-model" || doc.uri.scheme === "chat-editing-text-model") {
      const filePath = doc.uri.fsPath;
      const now = Date.now();

      const existing = this.snapshotOpenEvents.get(filePath);
      if (existing) {
        existing.count++;
        existing.timestamp = now;
      } else {
        this.snapshotOpenEvents.set(filePath, {
          timestamp: now,
          count: 1,
          uri: doc.uri // TODO Should we just let first writer wins for URI?
        });
      }

      // Trigger human checkpoint when whenever we see a snapshot open (before any changes are made -- debounce logic is handled in the triggerHumanCheckpoint method)
      console.log('[git-ai] AIEditManager: Snapshot open event detected for', filePath, 'scheme:', doc.uri.scheme, 'seen count:', this.snapshotOpenEvents.get(filePath)?.count, '- triggering human checkpoint');
      this.triggerHumanCheckpoint([filePath]);
    }
  }

  public handleCloseEvent(doc: vscode.TextDocument): void {
    // Clean up stable content cache for closed file:// documents
    if (doc.uri.scheme === "file") {
      const filePath = doc.uri.fsPath;
      this.stableFileContent.delete(filePath);
      const timer = this.stableContentTimers.get(filePath);
      if (timer) {
        clearTimeout(timer);
        this.stableContentTimers.delete(filePath);
      }
    }

    if (doc.uri.scheme === "chat-editing-snapshot-text-model" || doc.uri.scheme === "chat-editing-text-model") {
      console.log('[git-ai] AIEditManager: Snapshot close event detected for', doc);
      // console.log('[git-ai] AIEditManager: Snapshot close event detected, triggering human checkpoint');
      // const filePath = doc.uri.fsPath;
      // this.triggerHumanCheckpoint([filePath]);
    }
  }

  public getDirtyFiles(): { [filePath: string]: string } {
    // Return a map of absolute file paths to string content of any dirty files in the workspace
    const dirtyFiles = vscode.workspace.textDocuments.filter(doc => doc.isDirty && doc.uri.scheme === "file");
    return dirtyFiles.reduce((acc, doc) => {
      acc[doc.uri.fsPath] = doc.getText();
      return acc;
    }, {} as { [filePath: string]: string });
  }

  private evaluateSaveForCheckpoint(filePath: string): void {
    const saveInfo = this.pendingSaves.get(filePath);
    if (!saveInfo) {
      return;
    }

    const snapshotInfo = this.snapshotOpenEvents.get(filePath);

    console.log('[git-ai] AIEditManager: Evaluating save for checkpoint for', filePath, '- snapshot info:', snapshotInfo);

    // Check if we have 1+ valid snapshot open events within the debounce window
    let checkpointTriggered = false;

    if (snapshotInfo && snapshotInfo.count >= 1 && snapshotInfo.uri?.query) {
      // Check if the snapshot is fresh to avoid triggering AI checkpoints on stale snapshots
      const snapshotAge = Date.now() - snapshotInfo.timestamp;

      if (snapshotAge >= this.MAX_SNAPSHOT_AGE_MS) {
        console.log('[git-ai] AIEditManager: Snapshot is too old (' + Math.round(snapshotAge / 1000) + 's), skipping AI checkpoint for', filePath);
      } else {
        const storagePath = this.workspaceBaseStoragePath;
        if (!storagePath) {
          console.warn('[git-ai] AIEditManager: Missing workspace storage path, skipping AI checkpoint for', filePath);
        } else {
          try {
            const params = JSON.parse(snapshotInfo.uri.query);
            let sessionId = params.chatSessionId || params.sessionId;

            console.log("[git-ai] AIEditManager: Parsed snapshot params:", params);

            // "{"kind":"doc","documentId":"modified-file-entry::1","chatSessionResource":{"$mid":1,"external":"vscode-chat-session://local/MDFmNjJlNmItOTgxMi00OTY0LWI5YTYtYzRmZDBjZTE1ZmEy","path":"/MDFmNjJlNmItOTgxMi00OTY0LWI5YTYtYzRmZDBjZTE1ZmEy","scheme":"vscode-chat-session","authority":"local"}}"
            if (!sessionId && params.chatSessionResource) {
              // VS Code update includes the chatSessionResource object with the sessionId encoded in the path
              console.log("[git-ai] AIEditManager: Detected chatSessionResource, attempting to parse sessionId");
              sessionId = params.chatSessionResource.path ? Buffer.from(params.chatSessionResource.path.slice(1), 'base64').toString('utf-8') : undefined;
              console.log("[git-ai] AIEditManager: Parsed sessionId from chatSessionResource:", sessionId);
            }

            if (!sessionId && params.session && params.session.path && params.session.path.startsWith && params.session.path.startsWith('/')) {
              // VS Code update includes the sessionId encoded as Base64 
              console.log("[git-ai] AIEditManager: Detected session as object, decoding sessionId");
              sessionId = Buffer.from(params.session.path.slice(1), 'base64').toString('utf-8');
              console.log("[git-ai] AIEditManager: Parsed sessionId from Base64:", sessionId);
            }

            if (!sessionId) {
              console.warn('[git-ai] AIEditManager: Snapshot URI missing session id, skipping AI checkpoint for', filePath);
            } else {
              const workspaceFolder = vscode.workspace.getWorkspaceFolder(vscode.Uri.file(filePath));
              if (!workspaceFolder) {
                console.warn('[git-ai] AIEditManager: No workspace folder found for', filePath, '- skipping AI checkpoint');
              } else {
              const chatSessionsDir = path.join(storagePath, 'chatSessions');
              const jsonlPath = path.join(chatSessionsDir, `${sessionId}.jsonl`);
              const jsonPath = path.join(chatSessionsDir, `${sessionId}.json`);
              const chatSessionPath = fs.existsSync(jsonlPath) ? jsonlPath : jsonPath;
              console.log('[git-ai] AIEditManager: AI edit detected for', filePath, '- triggering AI checkpoint (sessionId:', sessionId, ', chatSessionPath:', chatSessionPath, ', workspaceFolder:', workspaceFolder.uri.fsPath, ')');
              
              // Get dirty files and ensure the saved file is included with its content from VS Code
              const dirtyFiles = this.getDirtyFiles();
              console.log('[git-ai] AIEditManager: Dirty files:', dirtyFiles);
              
              // Get the content of the saved file from VS Code (not from FS) to handle codespaces lag
              const savedFileDoc = vscode.workspace.textDocuments.find(doc => 
                doc.uri.fsPath === filePath && doc.uri.scheme === "file"
              );
              if (savedFileDoc) {
                dirtyFiles[filePath] = savedFileDoc.getText();
              }
              
              console.log('[git-ai] AIEditManager: Dirty files with saved file content:', dirtyFiles);
              this.checkpoint("ai", JSON.stringify({
                hook_event_name: "after_edit",
                chat_session_path: chatSessionPath,
                session_id: sessionId,
                edited_filepaths: [filePath],
                workspace_folder: workspaceFolder.uri.fsPath,
                dirty_files: dirtyFiles,
              }));
              checkpointTriggered = true;
              }
            }
          } catch (e) {
            console.error('[git-ai] AIEditManager: Unable to trigger AI checkpoint for', filePath, e);
          }
        }
      }
    }

    if (!checkpointTriggered) {
      console.log('[git-ai] AIEditManager: No AI pattern detected for', filePath, '- skipping checkpoint');
    }

    // Cleanup
    this.pendingSaves.delete(filePath);
    this.snapshotOpenEvents.delete(filePath);
  }

  /**
   * Trigger a human checkpoint with debouncing per file.
   * Debounce logic: trigger immediately, but skip files that were already checkpointed within the debounce window.
   */
  private triggerHumanCheckpoint(willEditFilepaths: string[]): void {
    if (!willEditFilepaths || willEditFilepaths.length === 0) {
      console.warn('[git-ai] AIEditManager: Cannot trigger human checkpoint without files');
      return;
    }

    // Filter out files that were recently checkpointed (within debounce window)
    const now = Date.now();
    const filesToCheckpoint = willEditFilepaths.filter(filePath => {
      const lastCheckpoint = this.lastHumanCheckpointAt.get(filePath);
      if (lastCheckpoint && (now - lastCheckpoint) < this.HUMAN_CHECKPOINT_DEBOUNCE_MS) {
        console.log('[git-ai] AIEditManager: Skipping file due to debounce:', filePath);
        return false;
      }
      return true;
    });

    if (filesToCheckpoint.length === 0) {
      console.log('[git-ai] AIEditManager: All files were recently checkpointed, skipping');
      return;
    }

    // Update last checkpoint time for files we're about to checkpoint
    filesToCheckpoint.forEach(filePath => {
      this.lastHumanCheckpointAt.set(filePath, now);
    });

    // Get dirty files
    const dirtyFiles = this.getDirtyFiles();

    // Add the files we're checkpointing to dirtyFiles (even if they're not dirty)
    // Use stable (pre-edit) content cache to avoid capturing AI edits that may already be in the buffer
    filesToCheckpoint.forEach(filePath => {
      const cachedContent = this.stableFileContent.get(filePath);
      if (cachedContent !== undefined) {
        dirtyFiles[filePath] = cachedContent;
      } else {
        const fileDoc = vscode.workspace.textDocuments.find(doc =>
          doc.uri.fsPath === filePath && doc.uri.scheme === "file"
        );
        if (fileDoc) {
          dirtyFiles[filePath] = fileDoc.getText();
        }
      }
    });

    // Find workspace folder
    const workspaceFolder = vscode.workspace.getWorkspaceFolder(vscode.Uri.file(filesToCheckpoint[0]))
      || vscode.workspace.workspaceFolders?.[0];

    if (!workspaceFolder) {
      console.warn('[git-ai] AIEditManager: No workspace folder found for human checkpoint');
      return;
    }

    console.log('[git-ai] AIEditManager: Triggering human checkpoint for files:', filesToCheckpoint);

    // Prepare hook input for human checkpoint (session ID is not reliable, so we skip it)
    const hookInput = JSON.stringify({
      hook_event_name: "before_edit",
      workspace_folder: workspaceFolder.uri.fsPath,
      will_edit_filepaths: filesToCheckpoint,
      dirty_files: dirtyFiles,
    });

    this.checkpoint("human", hookInput);
  }

  async checkpoint(author: "human" | "ai" | "ai_tab", hookInput: string): Promise<boolean> {
    if (author !== "ai_tab" && !this.legacyCopilotHooksEnabled) {
      console.log("[git-ai] AIEditManager: Skipping legacy human/ai checkpoint dispatch (native VS Code hooks active)");
      return true;
    }

    if (!(await this.checkGitAi())) {
      return false;
    }

    return new Promise<boolean>((resolve) => {
      let workspaceRoot: string | undefined;

      const activeEditor = vscode.window.activeTextEditor;
      if (activeEditor) {
        const documentUri = activeEditor.document.uri;
        // Try to get git repository root first, fallback to workspace folder
        const gitRepoRoot = getGitRepoRoot(documentUri);
        if (gitRepoRoot) {
          workspaceRoot = gitRepoRoot;
        } else {
          const workspaceFolder = vscode.workspace.getWorkspaceFolder(documentUri);
          if (workspaceFolder) {
            workspaceRoot = workspaceFolder.uri.fsPath;
          }
        }
      }

      if (!workspaceRoot) {
        // Try to get git repo root from first workspace folder
        const firstWorkspaceFolder = vscode.workspace.workspaceFolders?.[0];
        if (firstWorkspaceFolder) {
          const gitRepoRoot = getGitRepoRoot(firstWorkspaceFolder.uri);
          workspaceRoot = gitRepoRoot || firstWorkspaceFolder.uri.fsPath;
        }
      }

      if (!workspaceRoot) {
        console.warn('[git-ai] AIEditManager: No workspace root found, skipping checkpoint');
        resolve(false);
        return;
      }

      const args = ["checkpoint"];
      if (author === "ai_tab") {
        args.push("ai_tab");
      } else {
        args.push("github-copilot");
      }
      args.push("--hook-input", "stdin");

      console.log('[git-ai] AIEditManager: Spawning git-ai with args:', args);
      console.log('[git-ai] AIEditManager: Workspace root:', workspaceRoot);
      console.log('[git-ai] AIEditManager: Hook input:', hookInput);

      const proc = spawn(getGitAiBinary(), args, { cwd: workspaceRoot });

      let stdout = "";
      let stderr = "";

      proc.stdout.on("data", (data) => {
        stdout += data.toString();
      });

      proc.stderr.on("data", (data) => {
        stderr += data.toString();
      });

      proc.on("error", (error) => {
        console.error('[git-ai] AIEditManager: Checkpoint error:', error, stdout, stderr);
        vscode.window.showErrorMessage(
          "git-ai checkpoint error: " + error.message + " - " + stdout + " - " + stderr
        );
        resolve(false);
      });

      proc.on("close", (code) => {
        if (code !== 0) {
          console.error('[git-ai] AIEditManager: Checkpoint exited with code:', code, stdout, stderr);
          vscode.window.showErrorMessage(
            "git-ai checkpoint error: exited with code " + code + " - " + stdout + " - " + stderr
          );
          resolve(false);
        } else {
          const config = vscode.workspace.getConfiguration("gitai");
          if (config.get("enableCheckpointLogging")) {
            vscode.window.showInformationMessage(
              "Checkpoint created " + author
            );
          }
          resolve(true);
        }
      });

      if (hookInput) {
        proc.stdin.write(hookInput);
        proc.stdin.end();
      }
    });
  }

  async showGitAiUpdateRequiredMsg(detectedVersion: string) {
    const url = vscode.Uri.parse(GIT_AI_INSTALL_DOCS_URL);

    const choice = await vscode.window.showErrorMessage(
      `git-ai version ${detectedVersion} is no longer supported.`,
      'Update git-ai'
    );

    if (choice === 'Update git-ai') {
      await vscode.env.openExternal(url);
    }
  }

  async showGitAiNotInstalledMsg() {
    const url = vscode.Uri.parse(GIT_AI_INSTALL_DOCS_URL);

    const choice = await vscode.window.showInformationMessage(
      'git-ai is not installed.',
      'Install git-ai'
    );

    if (choice === 'Install git-ai') {
      await vscode.env.openExternal(url);
    }
  }

  async checkGitAi(): Promise<boolean> {
    if (this.gitAiVersion) {
      return true;
    }
    // TODO Consider only re-checking every X attempts
    return new Promise((resolve) => {
      exec("git-ai --version", (error, stdout, stderr) => {
        if (error) {
          if (!this.hasShownGitAiErrorMessage) {
            // Show startup notification
            vscode.window.showInformationMessage(
              "git-ai not installed. Visit https://github.com/git-ai-project/git-ai to install it."
            );
            this.hasShownGitAiErrorMessage = true;
          }
          // not installed. do nothing
          resolve(false);
        } else {
          const stdoutTrimmed = stdout.trim();
          // Extract strict semver (major.minor.patch) and ignore any trailing labels like "(debug)"
          const semverMatch = stdoutTrimmed.match(/\b\d+\.\d+\.\d+\b/);
          const detectedVersion = semverMatch ? semverMatch[0] : stdoutTrimmed.replace(/\s*\(.+\)\s*$/, "");

          if (!isVersionSatisfied(detectedVersion, MIN_GIT_AI_VERSION)) {
            if (!this.hasShownGitAiErrorMessage) {
              this.showGitAiUpdateRequiredMsg(detectedVersion);
              this.hasShownGitAiErrorMessage = true;
            }
            resolve(false);
            return;
          }

          // Save the version for later use
          this.gitAiVersion = detectedVersion;
          resolve(true);
        }
      });
    });
  }
}
