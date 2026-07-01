import * as assert from "assert";
import { shouldSkipLegacyCopilotHooks, VSCodeHookRuntime } from "../utils/vscode-hooks";

suite("VS Code Hook Gating", () => {
  const stableLocalHooksRuntime = (overrides: Partial<VSCodeHookRuntime> = {}): VSCodeHookRuntime => ({
    vscodeVersion: "1.109.3",
    appName: "Visual Studio Code",
    uriScheme: "vscode",
    appHost: "desktop",
    chatUseHooksEnabled: true,
    ...overrides,
  });

  test("skips legacy hooks only when stable local VS Code has native hooks enabled", () => {
    assert.strictEqual(shouldSkipLegacyCopilotHooks(stableLocalHooksRuntime()), true);
    assert.strictEqual(shouldSkipLegacyCopilotHooks(stableLocalHooksRuntime({ vscodeVersion: "1.109.4" })), true);
    assert.strictEqual(shouldSkipLegacyCopilotHooks(stableLocalHooksRuntime({ vscodeVersion: "1.110.0" })), true);
  });

  test("keeps legacy hooks below native hook version support", () => {
    assert.strictEqual(shouldSkipLegacyCopilotHooks(stableLocalHooksRuntime({ vscodeVersion: "1.109.2" })), false);
    assert.strictEqual(shouldSkipLegacyCopilotHooks(stableLocalHooksRuntime({ vscodeVersion: "1.108.0" })), false);
    assert.strictEqual(shouldSkipLegacyCopilotHooks(stableLocalHooksRuntime({ vscodeVersion: "1.109.3-alpha" })), false);
  });

  test("keeps legacy hooks when chat hooks are not enabled", () => {
    assert.strictEqual(shouldSkipLegacyCopilotHooks(stableLocalHooksRuntime({ chatUseHooksEnabled: false })), false);
    assert.strictEqual(shouldSkipLegacyCopilotHooks(stableLocalHooksRuntime({ chatUseHooksEnabled: undefined })), false);
  });

  test("keeps legacy hooks for VS Code Insiders", () => {
    assert.strictEqual(shouldSkipLegacyCopilotHooks(stableLocalHooksRuntime({
      appName: "Visual Studio Code - Insiders",
      uriScheme: "vscode-insiders",
      vscodeVersion: "1.110.0-insider",
    })), false);
  });

  test("keeps legacy hooks for WSL and other remote extension hosts", () => {
    assert.strictEqual(shouldSkipLegacyCopilotHooks(stableLocalHooksRuntime({ remoteName: "wsl" })), false);
    assert.strictEqual(shouldSkipLegacyCopilotHooks(stableLocalHooksRuntime({ remoteName: "ssh-remote" })), false);
  });

  test("keeps legacy hooks outside the desktop VS Code host", () => {
    assert.strictEqual(shouldSkipLegacyCopilotHooks(stableLocalHooksRuntime({ appHost: "vscode.dev" })), false);
    assert.strictEqual(shouldSkipLegacyCopilotHooks(stableLocalHooksRuntime({
      appName: "GitHub Codespaces",
      uriScheme: "vscode",
      appHost: "github.dev",
    })), false);
  });
});
