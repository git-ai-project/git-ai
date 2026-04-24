import { isVersionSatisfied } from "./semver";
import { MIN_VSCODE_NATIVE_HOOKS_VERSION } from "../consts";

/**
 * VS Code 1.109.3+ supports built-in Copilot hooks, so our extension should stop
 * emitting legacy before_edit/after_edit checkpoints to avoid duplicate attribution.
 *
 * However, in remote contexts (WSL, SSH, Codespaces) the native Copilot hooks fire
 * on the Local Extension Host where Copilot runs, but git-ai runs on the Remote
 * Extension Host. VS Code's Remote protocol does not relay these hooks across hosts,
 * so we must keep legacy hooks active in remote sessions.
 */
export function shouldSkipLegacyCopilotHooks(vscodeVersion: string, remoteName?: string): boolean {
  if (remoteName) {
    return false;
  }
  return isVersionSatisfied(vscodeVersion, MIN_VSCODE_NATIVE_HOOKS_VERSION);
}
