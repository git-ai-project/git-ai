import { execFile } from "child_process";
import * as fs from "fs";
import * as os from "os";
import * as vscode from "vscode";
import { Config } from "./config";

let resolvedPath: string | null = null;
let resolvePromise: Promise<string | null> | null = null;
let extensionMode: vscode.ExtensionMode | null = null;
let hasShownBinaryPathWarning = false;

/**
 * Call once at activation to pass in the extension context's mode.
 */
export function initBinaryResolver(mode: vscode.ExtensionMode): void {
  extensionMode = mode;
}

export function resetGitAiBinaryCache(): void {
  resolvedPath = null;
  resolvePromise = null;
  hasShownBinaryPathWarning = false;
}

/**
 * Resolve the full path to the `git-ai` binary using a login shell.
 * A configured binary path always wins. Otherwise this only runs in
 * development mode; in production the plain "git-ai" name is used directly.
 *
 * The result is cached after the first successful resolution.
 */
export function resolveGitAiBinary(): Promise<string | null> {
  const configuredPath = Config.getBinaryPath();
  if (configuredPath) {
    if (!fs.existsSync(configuredPath) && !hasShownBinaryPathWarning) {
      hasShownBinaryPathWarning = true;
      vscode.window.showWarningMessage(
        `git-ai: configured binary path does not exist: "${configuredPath}". Check the gitai.binaryPath setting.`
      );
    }
    resolvePromise = null;
    return Promise.resolve(configuredPath);
  }

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
  return Config.getBinaryPath() || resolvedPath || "git-ai";
}
