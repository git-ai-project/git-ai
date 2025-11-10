import * as vscode from "vscode";
import * as path from "path";
import { exec, spawn } from "child_process";
import { isVersionSatisfied } from "./utils/semver";
import { MIN_GIT_AI_VERSION, GIT_AI_INSTALL_DOCS_URL } from "./consts";

export class AIEditManager {
  private workspaceBaseStoragePath: string | null = null;
  private gitAiVersion: string | null = null;
  private hasShownGitAiErrorMessage = false;
  private lastHumanCheckpointAt: Date | null = null;
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

  constructor(context: vscode.ExtensionContext) {
    if (context.storageUri?.fsPath) {
      this.workspaceBaseStoragePath = path.dirname(context.storageUri.fsPath);
    } else {
      // No workspace active (extension will be re-activated when a workspace is opened)
      console.warn('[git-ai] No workspace storage URI available');
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

  public handleOpenEvent(doc: vscode.TextDocument): void {
    if (doc.uri.scheme === "chat-editing-snapshot-text-model") {
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

      console.log('[git-ai] AIEditManager: Snapshot open event tracked for', filePath, 'count:', this.snapshotOpenEvents.get(filePath)?.count);
    }
  }

  public handleCloseEvent(doc: vscode.TextDocument): void {
    if (doc.uri.scheme === "chat-editing-snapshot-text-model") {
      console.log('[git-ai] AIEditManager: Snapshot close event detected, triggering human checkpoint');
      this.checkpoint("human");
    }
  }

  public getDirtyFiles(): { [filePath: string]: string } {
    // Return a map of absolute file paths to string content of any dirty files in the workspace
    const dirtyFiles = vscode.workspace.textDocuments.filter(doc => doc.isDirty && doc.uri.scheme == "file");
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

    // Check if we have 1+ valid snapshot open events within the debounce window
    let checkpointTriggered = false;

    if (snapshotInfo && snapshotInfo.count >= 1 && snapshotInfo.uri?.query) {
      const storagePath = this.workspaceBaseStoragePath;
      if (!storagePath) {
        console.warn('[git-ai] AIEditManager: Missing workspace storage path, skipping AI checkpoint for', filePath);
      } else {
        try {
          const params = JSON.parse(snapshotInfo.uri.query);
          const sessionId = params.sessionId;
          const requestId = params.requestId;

          if (!sessionId || !requestId) {
            console.warn('[git-ai] AIEditManager: Snapshot URI missing session or request id, skipping AI checkpoint for', filePath);
          } else {
            const workspaceFolder = vscode.workspace.getWorkspaceFolder(vscode.Uri.file(filePath));
            if (!workspaceFolder) {
              console.warn('[git-ai] AIEditManager: No workspace folder found for', filePath, '- skipping AI checkpoint');
            } else {
              const chatSessionPath = path.join(storagePath, 'chatSessions', `${sessionId}.json`);
              console.log('[git-ai] AIEditManager: AI edit detected for', filePath, '- triggering AI checkpoint (sessionId:', sessionId, ', requestId:', requestId, ', chatSessionPath:', chatSessionPath, ', workspaceFolder:', workspaceFolder.uri.fsPath, ')');
              this.checkpoint("ai", JSON.stringify({
                chatSessionPath,
                sessionId,
                requestId,
                workspaceFolder: workspaceFolder.uri.fsPath,
                dirtyFiles: this.getDirtyFiles(),
              }));
              checkpointTriggered = true;
            }
          }
        } catch (e) {
          console.error('[git-ai] AIEditManager: Unable to trigger AI checkpoint for', filePath, e);
        }
      }
    }

    if (!checkpointTriggered) {
      console.log('[git-ai] AIEditManager: No AI pattern detected for', filePath, '- triggering human checkpoint');
      this.checkpoint("human");
    }

    // Cleanup
    this.pendingSaves.delete(filePath);
    this.snapshotOpenEvents.delete(filePath);
  }

  public triggerInitialHumanCheckpoint(): void {
    console.log('[git-ai] AIEditManager: Triggering initial human checkpoint');
    this.checkpoint("human");
  }

  async checkpoint(author: "human" | "ai" | "ai_tab", hookInput?: string): Promise<boolean> {
    if (!(await this.checkGitAi())) {
      return false;
    }

    // Throttle human checkpoints
    if (author === "human") {
      const now = new Date();
      if (this.lastHumanCheckpointAt && (now.getTime() - this.lastHumanCheckpointAt.getTime()) < this.HUMAN_CHECKPOINT_DEBOUNCE_MS) {
        console.log('[git-ai] AIEditManager: Skipping human checkpoint due to debounce');
        return false;
      }
      this.lastHumanCheckpointAt = now;
    }

    return new Promise<boolean>((resolve, reject) => {
      let workspaceRoot: string | undefined;

      const activeEditor = vscode.window.activeTextEditor;
      if (activeEditor) {
        const documentUri = activeEditor.document.uri;
        const workspaceFolder = vscode.workspace.getWorkspaceFolder(documentUri);
        if (workspaceFolder) {
          workspaceRoot = workspaceFolder.uri.fsPath;
        }
      }

      if (!workspaceRoot) {
        workspaceRoot = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
      }

      if (!workspaceRoot) {
        console.warn('[git-ai] AIEditManager: No workspace root found, skipping checkpoint');
        resolve(false);
        return;
      }

      const args = ["checkpoint"];
      if (author === "ai") {
        args.push("github-copilot");
      }
      if (hookInput) {
        args.push("--hook-input", "stdin");
      }

      const proc = spawn("git-ai", args, { cwd: workspaceRoot });

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
              "git-ai not installed. Visit https://github.com/acunniffe/git-ai to install it."
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