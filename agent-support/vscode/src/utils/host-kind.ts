import * as vscode from "vscode";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";

export type IDEHostConfiguration = {
  kind: IDEHostKind;
  appName: string;
  uriScheme: string;
  execPath: string;
}

export const IDEHostKindCursor = 'cursor' as const;
export const IDEHostKindWindsurf = 'windsurf' as const;
export const IDEHostKindVSCode = 'vscode' as const;
export const IDEHostKindUnknown = 'unknown' as const;

export type IDEHostKind =
  | typeof IDEHostKindCursor
  | typeof IDEHostKindWindsurf
  | typeof IDEHostKindVSCode
  | typeof IDEHostKindUnknown;

/**
 * Loads the list of IDE names that should be treated as VSCode by checking the configuration file.
 */
function loadVscodeIdeNames(): string[] {
  try {
    const filePath = path.join(os.homedir(), ".git-ai", "VSCODE_IDE_NAMES");
    const content = fs.readFileSync(filePath, "utf-8");
    return content
      .split("\n")
      .map((line: string) => line.trim().toLowerCase())
      .filter((line: string) => line.length > 0);
  } catch {
    return [];
  }
}

export function detectIDEHost(): IDEHostConfiguration {
  const appName = (vscode.env.appName ?? "").toLowerCase();
  const uriScheme = (vscode.env.uriScheme ?? "").toLowerCase();
  const execPath = (process.execPath ?? "").toLowerCase();

  const has = (s: string) => appName.includes(s) || uriScheme === s || execPath.includes(`${path.sep}${s}`);

  let kind: IDEHostKind =
    has("cursor") ? "cursor" :
    has("windsurf") ? "windsurf" :
    has("vscodium") || uriScheme === "vscode-insiders" || uriScheme === "vscode" || appName.includes("visual studio code") ? "vscode" :
    "unknown";

  // If we don't recognize the IDE, check if it's a custom VSCode-like IDE that should be treated as VSCode by checking the configuration file.
  if (kind === "unknown") {
    const vscodeIdeNames = loadVscodeIdeNames();
    if (vscodeIdeNames.length > 0) {
      console.log('[git-ai] Checking for VSCode-like IDE: ', vscodeIdeNames);
      if (vscodeIdeNames.some(has)) {
        kind = "vscode";
        console.log('[git-ai] Found VSCode-like IDE: ', kind);
      }
    }
  } else {
    console.log('[git-ai] Recognized IDE: ', kind);
  }

  return { kind, appName: vscode.env.appName, uriScheme: vscode.env.uriScheme, execPath: process.execPath };
}
