import * as assert from "assert";
import * as fs from "node:fs/promises";
import * as os from "node:os";
import * as path from "node:path";
import * as vscode from "vscode";
import { AIEditManager } from "../ai-edit-manager";
import { AITabEditManager } from "../ai-tab-edit-manager";
import { IDEHostKind } from "../utils/host-kind";

suite("AI tab checkpoint attribution", () => {
  for (const [kind, expectedTool] of [
    ["cursor", "cursor-tab"],
    ["vscode", "github-copilot-tab"],
  ] as const satisfies ReadonlyArray<readonly [IDEHostKind, string]>) {
    test(`${kind} preserves its completion tool in both checkpoints`, async () => {
      const directory = await fs.mkdtemp(path.join(os.tmpdir(), "git-ai-tab-"));
      try {
        const filePath = path.join(directory, "completion.ts");
        await fs.writeFile(filePath, "const answer = ");
        const document = await vscode.workspace.openTextDocument(filePath);
        const checkpoints: Array<{ preset: string; payload: Record<string, unknown> }> = [];
        const editManager = {
          getDirtyFiles: () => ({}),
          checkpoint: async (preset: string, input: string) => {
            checkpoints.push({ preset, payload: JSON.parse(input) });
          },
        } as unknown as AIEditManager;
        const manager = new AITabEditManager(
          { subscriptions: [] } as unknown as vscode.ExtensionContext,
          { kind, appName: kind, uriScheme: kind, execPath: "" },
          editManager,
        );

        manager.beforeHook([]);
        const afterContent = "const answer = 42;";
        manager.handleDocumentContentChangeEvent({
          document: {
            uri: document.uri,
            getText: () => afterContent,
          },
          contentChanges: [],
        } as unknown as vscode.TextDocumentChangeEvent);
        await manager.afterHook(undefined);

        assert.strictEqual(checkpoints.length, 2);
        assert.deepStrictEqual(checkpoints.map(({ preset }) => preset), ["ai_tab", "ai_tab"]);
        assert.deepStrictEqual(checkpoints.map(({ payload }) => payload.tool), [expectedTool, expectedTool]);
        assert.deepStrictEqual(checkpoints.map(({ payload }) => payload.hook_event_name), ["before_edit", "after_edit"]);
        assert.deepStrictEqual(checkpoints[0].payload.dirty_files, { [document.uri.fsPath]: "const answer = " });
        assert.deepStrictEqual(checkpoints[1].payload.dirty_files, { [document.uri.fsPath]: afterContent });
      } finally {
        await fs.rm(directory, { recursive: true, force: true });
      }
    });
  }
});
