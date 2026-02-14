import * as assert from "assert";
import { shouldSkipLegacyCopilotHooks } from "../utils/vscode-hooks";

suite("VS Code Hook Gating", () => {
  test("skips legacy hooks at and above 1.109.3", () => {
    assert.strictEqual(shouldSkipLegacyCopilotHooks("1.109.3"), true);
    assert.strictEqual(shouldSkipLegacyCopilotHooks("1.109.4"), true);
    assert.strictEqual(shouldSkipLegacyCopilotHooks("1.110.0"), true);
    assert.strictEqual(shouldSkipLegacyCopilotHooks("1.110.0-insider"), true);
  });

  test("keeps legacy hooks below 1.109.3", () => {
    assert.strictEqual(shouldSkipLegacyCopilotHooks("1.109.2"), false);
    assert.strictEqual(shouldSkipLegacyCopilotHooks("1.108.0"), false);
    assert.strictEqual(shouldSkipLegacyCopilotHooks("1.109.3-alpha"), false);
  });
});
