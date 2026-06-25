import { isVersionSatisfied } from "./semver";
import { MIN_VSCODE_NATIVE_HOOKS_VERSION } from "../consts";

export type VSCodeHookRuntime = {
  vscodeVersion: string;
  appName?: string;
  uriScheme?: string;
  remoteName?: string;
  appHost?: string;
  chatUseHooksEnabled?: boolean;
};

/**
 * Keep legacy Copilot detection unless native hooks are known to be active in a
 * local, stable VS Code host. Version support alone is not enough: Insiders and
 * remote extension hosts can report a new VS Code version before the native hook
 * path is usable for attribution.
 */
export function shouldSkipLegacyCopilotHooks(runtime: VSCodeHookRuntime): boolean {
  if (!isVersionSatisfied(runtime.vscodeVersion, MIN_VSCODE_NATIVE_HOOKS_VERSION)) {
    return false;
  }

  if (runtime.chatUseHooksEnabled !== true) {
    return false;
  }

  if (runtime.remoteName && runtime.remoteName.trim().length > 0) {
    return false;
  }

  const appHost = (runtime.appHost ?? "").toLowerCase();
  if (appHost && appHost !== "desktop") {
    return false;
  }

  const appName = (runtime.appName ?? "").toLowerCase();
  const uriScheme = (runtime.uriScheme ?? "").toLowerCase();
  if (appName.includes("insiders") || uriScheme === "vscode-insiders") {
    return false;
  }

  return uriScheme === "vscode" || appName.includes("visual studio code");
}
