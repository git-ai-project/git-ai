import * as vscode from "vscode";
import { AIEditManager } from "./ai-edit-manager";
import { detectIDEHost, IDEHostKindVSCode } from "./utils/host-kind";
import { AITabEditManager } from "./ai-tab-edit-manager";
import { Config } from "./utils/config";

export function activate(context: vscode.ExtensionContext) {
  console.log('[git-ai] extension activated');

  const ideHostCfg = detectIDEHost();

  const aiEditManager = new AIEditManager(context);

  if (Config.isAiTabTrackingEnabled()) {
    const aiTabEditManager = new AITabEditManager(context, ideHostCfg, aiEditManager);
    const aiTabTrackingEnabled = aiTabEditManager.enableIfSupported();

    if (aiTabTrackingEnabled) {
      console.log('[git-ai] Tracking document content changes for AI tab completion detection');
      vscode.window.showInformationMessage('git-ai: AI tab tracking is enabled (experimental)');
      context.subscriptions.push(
        vscode.workspace.onDidChangeTextDocument((event) => {
          aiTabEditManager.handleDocumentContentChangeEvent(event);
        })
      );
    }
  }

  if (ideHostCfg.kind == IDEHostKindVSCode) {
    console.log('[git-ai] Using VS Code/Copilot detection strategy');
    // Trigger initial human checkpoint
    aiEditManager.triggerInitialHumanCheckpoint();

    // Save event
    context.subscriptions.push(
      vscode.workspace.onDidSaveTextDocument((doc) => {
        aiEditManager.handleSaveEvent(doc);
      })
    );

    // Open event
    context.subscriptions.push(
      vscode.workspace.onDidOpenTextDocument((doc) => {
        aiEditManager.handleOpenEvent(doc);
      })
    );

    // Close event
    context.subscriptions.push(
      vscode.workspace.onDidCloseTextDocument((doc) => {
        aiEditManager.handleCloseEvent(doc);
      })
    );
  }
}

export function deactivate() { }
