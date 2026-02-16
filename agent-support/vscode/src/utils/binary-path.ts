import { execFile } from "child_process";
import * as os from "os";
import * as vscode from "vscode";

let resolvedPath: string | null = null;
let resolvePromise: Promise<string | null> | null = null;
let extensionMode: vscode.ExtensionMode | null = null;

/**
 * Call once at activation to pass in the extension context's mode.
 */
export function initBinaryResolver(mode: vscode.ExtensionMode): void {
  extensionMode = mode;
}

/**
 * Resolve the full path to the `git-ai` binary using a login shell.
 * Only runs in development mode — in production the plain "git-ai" name
 * is used directly (relies on the process PATH).
 *
 * The result is cached after the first successful resolution.
 */
export function resolveGitAiBinary(): Promise<string | null> {
  // Skip shell resolution in production — just use "git-ai"
  if (extensionMode !== vscode.ExtensionMode.Development) {
    return Promise.resolve(null);
  }

  if (resolvedPath) {
    return Promise.resolve(resolvedPath);
  }
  if (resolvePromise) {
    return resolvePromise;
  }

  resolvePromise = new Promise((resolve) => {
    const platform = os.platform();

    if (platform === "win32") {
      // Windows: use `where git-ai`
      execFile("where", ["git-ai"], (err, stdout) => {
        if (err || !stdout.trim()) {
          console.log("[git-ai] Could not resolve git-ai binary via 'where'");
          resolve(null);
        } else {
          // `where` can return multiple lines; take the first
          resolvedPath = stdout.trim().split(/\r?\n/)[0];
          console.log("[git-ai] Resolved binary path:", resolvedPath);
          resolve(resolvedPath);
        }
      });
    } else {
      // macOS/Linux: spawn a login shell so the user's profile is sourced
      const shell = process.env.SHELL || "/bin/bash";
      execFile(shell, ["-ilc", "which git-ai"], { timeout: 5000 }, (err, stdout) => {
        if (err || !stdout.trim()) {
          console.log("[git-ai] Could not resolve git-ai binary via login shell");
          resolve(null);
        } else {
          resolvedPath = stdout.trim();
          console.log("[git-ai] Resolved binary path:", resolvedPath);
          resolve(resolvedPath);
        }
      });
    }
  });

  return resolvePromise;
}

/**
 * Get the resolved git-ai binary path, or fall back to just "git-ai"
 * (which relies on the current process PATH).
 */
export function getGitAiBinary(): string {
  return resolvedPath || "git-ai";
}
