import * as vscode from "vscode";
import debounce from "lodash.debounce";
import { exec } from "child_process";

interface ChangeEvent {
  timestamp: number;
  contentChanges: readonly vscode.TextDocumentContentChangeEvent[];
  isPaste: boolean;
}

const showCheckpointMessage = () => {
  const config = vscode.workspace.getConfiguration("cursorGitAi");
  return Boolean(config.get("enableCheckpointLogging"));
};

class AIDetector {
  private recentChanges: ChangeEvent[] = [];
  private lastPasteTime = 0;
  private lastUndoTime = 0;
  private lastRedoTime = 0;
  private readonly PASTE_THRESHOLD = 50; // ms
  private readonly UNDO_REDO_THRESHOLD = 50; // ms
  private readonly CHANGE_HISTORY_WINDOW = 2000; // ms
  private readonly MIN_LINES_FOR_AI = 2; // Minimum lines to consider AI insertion
  private readonly MIN_CHARS_PER_LINE = 20; // Minimum chars per line to consider meaningful
  private debouncedHumanEdit: (() => void) | null = null;
  private humanEditTimeout: NodeJS.Timeout | null = null;
  private aiDetectedRecently = false;

  constructor() {
    // Register our paste command that delegates to the original
    vscode.commands.registerCommand(
      "cursor-git-ai.pasteWithDetection",
      async () => {
        this.lastPasteTime = Date.now();
        // Execute the original paste functionality
        await vscode.commands.executeCommand(
          "editor.action.clipboardPasteAction"
        );
      }
    );

    // Register undo command
    vscode.commands.registerCommand(
      "cursor-git-ai.undoWithDetection",
      async () => {
        this.lastUndoTime = Date.now();
        // Execute the original undo functionality
        await vscode.commands.executeCommand("undo");
      }
    );

    // Register redo command
    vscode.commands.registerCommand(
      "cursor-git-ai.redoWithDetection",
      async () => {
        this.lastRedoTime = Date.now();
        // Execute the original redo functionality
        await vscode.commands.executeCommand("redo");
      }
    );
  }

  private isRecentPaste(): boolean {
    return Date.now() - this.lastPasteTime < this.PASTE_THRESHOLD;
  }

  private isRecentUndo(): boolean {
    return Date.now() - this.lastUndoTime < this.UNDO_REDO_THRESHOLD;
  }

  private isRecentRedo(): boolean {
    return Date.now() - this.lastRedoTime < this.UNDO_REDO_THRESHOLD;
  }

  private isRecentUndoOrRedo(): boolean {
    return this.isRecentUndo() || this.isRecentRedo();
  }

  private analyzeContentChanges(
    changes: readonly vscode.TextDocumentContentChangeEvent[]
  ): {
    totalLines: number;
    avgCharsPerLine: number;
    hasNewlines: boolean;
    isBulkInsertion: boolean;
  } {
    let totalLines = 0;
    let totalChars = 0;
    let hasNewlines = false;
    let isBulkInsertion = false;

    for (const change of changes) {
      const text = change.text;
      const lines = text.split("\n");
      totalLines += lines.length;
      totalChars += text.length;

      if (text.includes("\n")) {
        hasNewlines = true;
      }

      // Check if this looks like a bulk insertion (multiple lines with substantial content)
      if (lines.length >= this.MIN_LINES_FOR_AI) {
        const avgCharsPerLine = text.length / lines.length;
        if (avgCharsPerLine >= this.MIN_CHARS_PER_LINE) {
          isBulkInsertion = true;
        }
      }
    }

    const avgCharsPerLine = totalLines > 0 ? totalChars / totalLines : 0;

    return {
      totalLines,
      avgCharsPerLine,
      hasNewlines,
      isBulkInsertion,
    };
  }

  private normalizeContent(text: string): string {
    return text
      .replace(/\s+/g, "") // Remove all whitespace (spaces, tabs, newlines)
      .replace(/[;()]/g, "") // Remove semicolons and parentheses
      .toLowerCase(); // Normalize case for comparison
  }

  private isFormattingChange(
    changes: readonly vscode.TextDocumentContentChangeEvent[],
    document: vscode.TextDocument
  ): boolean {
    for (const change of changes) {
      const beforeText =
        change.rangeLength > 0 ? document.getText(change.range) : "";
      const beforeNormalized = this.normalizeContent(beforeText);
      const afterNormalized = this.normalizeContent(change.text);

      // If the normalized content is the same, it's just formatting
      if (beforeNormalized === afterNormalized) {
        return true;
      }
    }
    return false;
  }

  private isLikelyAIInsertion(
    changes: readonly vscode.TextDocumentContentChangeEvent[],
    document: vscode.TextDocument
  ): boolean {
    // If it's a recent paste, undo, or redo, it's probably not AI
    if (this.isRecentPaste() || this.isRecentUndoOrRedo()) {
      return false;
    }

    // Check if this is just a formatting change
    if (this.isFormattingChange(changes, document)) {
      return false;
    }

    const analysis = this.analyzeContentChanges(changes);

    // Heuristic 1: Bulk insertion with multiple lines
    if (analysis.isBulkInsertion) {
      return true;
    }

    // Heuristic 2: Multiple lines with substantial content
    if (
      analysis.totalLines >= this.MIN_LINES_FOR_AI &&
      analysis.avgCharsPerLine >= this.MIN_CHARS_PER_LINE
    ) {
      return true;
    }

    // Heuristic 3: Single large insertion (could be AI completing a line)
    if (
      changes.length === 1 &&
      changes[0].text.length > 50 &&
      !this.isRecentPaste() &&
      !this.isRecentUndoOrRedo()
    ) {
      return true;
    }

    return false;
  }

  public async processChange(
    event: vscode.TextDocumentChangeEvent
  ): Promise<void> {
    const changeEvent: ChangeEvent = {
      timestamp: Date.now(),
      contentChanges: event.contentChanges,
      isPaste: this.isRecentPaste(),
    };

    // Add to recent changes
    this.recentChanges.push(changeEvent);

    // Clean up old changes
    this.recentChanges = this.recentChanges.filter(
      (change) => Date.now() - change.timestamp < this.CHANGE_HISTORY_WINDOW
    );

    // Check if this looks like AI insertion
    const isAI = this.isLikelyAIInsertion(event.contentChanges, event.document);

    if (isAI) {
      // Cancel any pending human edit notification
      this.cancelHumanEditNotification();
      this.aiDetectedRecently = true;
      await this.onAIDetected(event);

      // Reset the flag after 4.5 seconds to cover the full human edit timeout period plus a small buffer
      // This prevents race conditions where human edits could trigger during the 4-second timeout window
      setTimeout(() => {
        this.aiDetectedRecently = false;
      }, 4500);
    } else {
      // Only trigger human edit if AI hasn't been detected recently
      if (!this.aiDetectedRecently) {
        this.triggerHumanEditNotification();
      } else {
        // Log when human edits are suppressed due to recent AI detection
        if (showCheckpointMessage()) {
          console.log("Human edit suppressed due to recent AI detection");
        }
      }
    }
  }

  private cancelHumanEditNotification(): void {
    if (this.humanEditTimeout) {
      clearTimeout(this.humanEditTimeout);
      this.humanEditTimeout = null;
    }
  }

  private triggerHumanEditNotification(): void {
    // Cancel any existing timeout
    this.cancelHumanEditNotification();

    this.humanEditTimeout = setTimeout(() => {
      checkpoint("human");
      this.humanEditTimeout = null;
    }, 4000);
  }

  private async onAIDetected(
    event: vscode.TextDocumentChangeEvent
  ): Promise<void> {
    const analysis = this.analyzeContentChanges(event.contentChanges);

    // Save all open documents before creating checkpoint
    await this.saveAllOpenDocuments();

    checkpoint("ai");
    // Only show notification if enabled in settings
    if (showCheckpointMessage()) {
      vscode.window.showInformationMessage(
        `AI Code Insertion Detected! Added ${
          analysis.totalLines
        } lines with ${Math.round(analysis.avgCharsPerLine)} chars per line.`
      );
    }
  }

  private async saveAllOpenDocuments(): Promise<void> {
    try {
      // Get all open text editors
      const openEditors = vscode.window.visibleTextEditors;

      // Save all documents that have unsaved changes
      const savePromises = openEditors
        .filter((editor) => editor.document.isDirty)
        .map((editor) => editor.document.save());

      if (savePromises.length > 0) {
        await Promise.all(savePromises);
        if (showCheckpointMessage()) {
          console.log(
            `Saved ${savePromises.length} open documents before AI checkpoint`
          );
        }
      }
    } catch (error) {
      console.error("Error saving documents:", error);
    }
  }
}

export function activate(context: vscode.ExtensionContext) {
  // Check if git-ai CLI is installed
  exec("git-ai --version", (error, stdout, stderr) => {
    if (error) {
      // Show startup notification
      vscode.window.showInformationMessage(
        "git-ai not installed. Visit https://github.com/acunniffe/git-ai to install it."
      );
      // not installed. do nothing
    } else {
      const aiDetector = new AIDetector();
      // Listen for text document changes
      const textDocumentChangeDisposable =
        vscode.workspace.onDidChangeTextDocument(async (event) => {
          await aiDetector.processChange(event);
        });

      context.subscriptions.push(textDocumentChangeDisposable);
      // Show startup notification
      vscode.window.showInformationMessage(
        `🤖 AI Code Detector is now active!`
      );
    }
  });
}

export function checkpoint(author: "human" | "ai") {
  return new Promise<boolean>((resolve, reject) => {
    // Get the workspace root directory for the current active editor
    let workspaceRoot: string | undefined;

    // Try to get workspace from active editor first
    const activeEditor = vscode.window.activeTextEditor;
    if (activeEditor) {
      const documentUri = activeEditor.document.uri;
      const workspaceFolder = vscode.workspace.getWorkspaceFolder(documentUri);
      if (workspaceFolder) {
        workspaceRoot = workspaceFolder.uri.fsPath;
      }
    }

    // Fallback to first workspace folder if no active editor or workspace folder found
    if (!workspaceRoot) {
      workspaceRoot = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
    }

    if (!workspaceRoot) {
      vscode.window.showErrorMessage("No workspace root found");
      resolve(false);
      return;
    }

    exec(
      `git-ai checkpoint ${author === "ai" ? "--author 'Cursor'" : ""}`,
      { cwd: workspaceRoot },
      (error, stdout, stderr) => {
        if (error) {
          if (showCheckpointMessage()) {
            vscode.window.showInformationMessage(
              "Error with checkpoint: " + error.message
            );
          }
          resolve(false);
        } else {
          if (showCheckpointMessage()) {
            vscode.window.showInformationMessage(
              "Checkpoint created " + author
            );
          }
          resolve(true);
        }
      }
    );
  });
}

// This method is called when your extension is deactivated
export function deactivate() {}
