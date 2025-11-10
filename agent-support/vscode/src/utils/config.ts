import * as vscode from "vscode";

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
}


