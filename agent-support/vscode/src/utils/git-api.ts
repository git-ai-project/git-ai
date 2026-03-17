import * as vscode from "vscode";
import * as path from "node:path";

/** Minimal structural type for a VS Code Git extension repository. */
export interface GitRepository {
  rootUri: vscode.Uri;
  state: {
    HEAD?: {
      commit?: string;
    };
  };
}

/**
 * Get the VS Code Git extension API (v1).
 * Returns undefined if the extension is not available.
 */
function getGitAPI() {
  return vscode.extensions
    .getExtension("vscode.git")
    ?.exports.getAPI(1) as { repositories: GitRepository[] } | undefined;
}

/**
 * Find the Git repository that contains the given file.
 * Uses a path-separator guard to prevent false prefix matches
 * (e.g. `/project-extra/file.ts` against repo root `/project`).
 */
export function findRepoForFile(fileUri: vscode.Uri): GitRepository | undefined {
  const git = getGitAPI();
  if (!git) {
    return undefined;
  }

  const filePath = fileUri.fsPath;
  return git.repositories
    .filter((r) => {
      const root = r.rootUri.fsPath;
      return filePath === root || filePath.startsWith(root + path.sep);
    })
    .sort((a, b) => b.rootUri.fsPath.length - a.rootUri.fsPath.length)[0];
}

/**
 * Get the git repository root directory for a file.
 * Convenience wrapper around findRepoForFile().
 * Returns null if the Git extension is not available or the file is not in a repo.
 */
export function getGitRepoRoot(fileUri: vscode.Uri): string | null {
  return findRepoForFile(fileUri)?.rootUri.fsPath ?? null;
}
