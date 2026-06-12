import * as vscode from "vscode";

export type BlameMode = 'off' | 'line' | 'all';

export class Config {
  private static getRoot(): vscode.WorkspaceConfiguration {
    return vscode.workspace.getConfiguration("gitai");
  }

  static isCheckpointLoggingEnabled(): boolean {
    return !!this.getRoot().get<boolean>("enableCheckpointLogging");
  }

  static isAiTabTrackingEnabled(): boolean {
    return !!this.getRoot().get<boolean>("experiments.aiTabTracking");
  }

  static getBinaryPath(): string | null {
    const binaryPath = this.getRoot().get<string>("binaryPath")?.trim();
    return binaryPath || null;
  }

  static getBlameMode(): BlameMode {
    const mode = this.getRoot().get<string>("blameMode");
    if (mode === 'off' || mode === 'line' || mode === 'all') {
      return mode;
    }
    return 'line'; // default
  }

  static async setBlameMode(mode: BlameMode): Promise<void> {
    await this.getRoot().update("blameMode", mode, vscode.ConfigurationTarget.Global);
  }
}

