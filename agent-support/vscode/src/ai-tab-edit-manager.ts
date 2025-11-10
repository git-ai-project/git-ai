import * as vscode from "vscode";
import { AIEditManager } from "./ai-edit-manager";
import { IDEHostConfiguration } from "./utils/host-kind";
import { TAB_AI_COMPLETION_COMMANDS } from "./consts";

export class AITabEditManager {
  private context: vscode.ExtensionContext;
  private ideHostConfig: IDEHostConfiguration;
  private aiEditManager: AIEditManager;
  private registration: vscode.Disposable | undefined;
  private restoring = false; // guards against re-entrancy during re-register
  private beforeCompletionFileStates: {[filePath: string]: string} | null = null;
  private lastDocumentChangeEvent: vscode.TextDocumentChangeEvent | null = null;

  constructor(context: vscode.ExtensionContext, ideHostConfig: IDEHostConfiguration, aiEditManager: AIEditManager) {
    this.context = context;
    this.ideHostConfig = ideHostConfig;
    this.aiEditManager = aiEditManager;
  }

  enableIfSupported(): boolean {
    if (this.isSupportedIDEHost()) {
      console.log(`[git-ai] Enabling AI tab detection for ${this.ideHostConfig.kind}`);
      this.registration = this.registerOverride();
      return true;
    }
    console.log(`[git-ai] AI tab detection not supported for ${this.ideHostConfig.kind}`);
    return false;
  }

  handleDocumentContentChangeEvent(event: vscode.TextDocumentChangeEvent): void {
    console.log('[git-ai] Document content change event', event);
    // TODO Apply some basic filtering against events that are not relevant to AI tab completion
    this.lastDocumentChangeEvent = event;
  }

  beforeHook(args: any[]) {
    // TODO Anything we should track here?
    console.log('[git-ai] before ai tab completion accepted', args);
    this.beforeCompletionFileStates = {};
    for (const doc of vscode.workspace.textDocuments) {
      if (doc.uri.scheme != "file") {
        continue;
      }
      this.beforeCompletionFileStates[doc.uri.fsPath] = doc.getText();
    }
  }

  async afterHook(result: unknown) {
    console.log('[git-ai] after ai tab completion accepted', result);
    const last = this.lastDocumentChangeEvent;
    if (!last) {
      console.log('[git-ai] No last document change event to inspect');
      return;
    }
    if (!this.beforeCompletionFileStates) {
      console.log('[git-ai] No before completion file states to inspect');
      return;
    }
    const afterContent = last.document.getText();
    let beforeContent: string | null = null;

    for (const [filePath, content] of Object.entries(this.beforeCompletionFileStates)) {
      if (filePath == last.document.uri.fsPath) {
        beforeContent = content;
        break;
      }
    }
    if (!beforeContent) {
      console.log('[git-ai] No before content found for', last.document.uri.fsPath);
      return;
    }

    // Before edit checkpoint
    await this.aiEditManager.checkpoint("ai_tab", JSON.stringify({
      hook_event_name: 'before_edit',
      tool: 'github-copilot-tab',
      model: 'default',
      will_edit_filepaths: [last.document.uri.fsPath],
      dirty_files: {
        ...this.aiEditManager.getDirtyFiles(),
        [last.document.uri.fsPath]: beforeContent,
      }
    }));

    // After edit checkpoint
    await this.aiEditManager.checkpoint("ai_tab", JSON.stringify({
      hook_event_name: 'after_edit',
      tool: 'github-copilot-tab',
      model: 'default',
      edited_filepaths: [last.document.uri.fsPath],
      dirty_files: {
        ...this.aiEditManager.getDirtyFiles(),
        [last.document.uri.fsPath]: afterContent,
      }
    }));

    this.beforeCompletionFileStates = null;
  }

  registerOverride() {
    const disp = vscode.commands.registerCommand(this.getTabAcceptedCommand(), async (...args: any[]) => {
      // If we're currently re-registering (restoring), just bail to avoid loops.
      if (this.restoring) {
        return;
      }

      // Unregister our override so executing the same command calls the previous handler.
      try {
        this.registration?.dispose();
        this.registration = undefined;
      } catch { /* ignore */ }

      try {
        this.beforeHook(args);

        // Call the "original" command implementation (the previously registered handler).
        const result = await vscode.commands.executeCommand(this.getTabAcceptedCommand(), ...args);

        this.afterHook(result);
        return result;
      } finally {
        // Always restore our override so future executions flow through us again.
        try {
          this.restoring = true;
          this.registration = this.registerOverride();
        } finally {
          this.restoring = false;
        }
      }
    });

    // Keep it in extension subscriptions so VS Code cleans up on deactivate.
    this.context.subscriptions.push(disp);
    return disp;
  }

  isSupportedIDEHost(): boolean {
    return TAB_AI_COMPLETION_COMMANDS[this.ideHostConfig.kind] !== undefined;
  }

  getTabAcceptedCommand(): string {
    let command = TAB_AI_COMPLETION_COMMANDS[this.ideHostConfig.kind];
    if (!command) {
      throw new Error(`Unsupported IDE host kind: ${this.ideHostConfig.kind}`);
    }
    return command;
  }
}