import { isVersionSatisfied } from "./semver";
import { MIN_VSCODE_NATIVE_HOOKS_VERSION } from "../consts";

/**
 * VS Code 1.109.3+ supports built-in Copilot hooks, so our extension should stop
 * emitting legacy before_edit/after_edit checkpoints to avoid duplicate attribution.
 */
export function shouldSkipLegacyCopilotHooks(vscodeVersion: string): boolean {
  return isVersionSatisfied(vscodeVersion, MIN_VSCODE_NATIVE_HOOKS_VERSION);
}
