import * as vscode from "vscode";
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

export function detectIDEHost(): IDEHostConfiguration {
  const appName = (vscode.env.appName ?? "").toLowerCase();
  const uriScheme = (vscode.env.uriScheme ?? "").toLowerCase();
  const execPath = (process.execPath ?? "").toLowerCase();

  const has = (s: string) => appName.includes(s) || uriScheme === s || execPath.includes(`${path.sep}${s}`);

  const kind: IDEHostKind =
    has("cursor") ? "cursor" :
    has("windsurf") ? "windsurf" :
    has("vscodium") || uriScheme === "vscode-insiders" || uriScheme === "vscode" || appName.includes("visual studio code") ? "vscode" :
    "unknown";

  return { kind, appName: vscode.env.appName, uriScheme: vscode.env.uriScheme, execPath: process.execPath };
}
