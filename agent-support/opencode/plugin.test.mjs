import assert from "node:assert/strict"
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { test } from "node:test"
import { pathToFileURL } from "node:url"
import ts from "typescript"

test("installed plugin loads without node_modules and registers both APIs", async () => {
  const source = await readFile(new URL("./git-ai.ts", import.meta.url), "utf8")
  const compiled = ts.transpileModule(
    source.replace("__GIT_AI_BINARY_PATH__", "/usr/bin/git-ai"),
    { compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022 } },
  ).outputText
  const directory = await mkdtemp(join(tmpdir(), "git-ai-opencode-plugin-"))

  try {
    const pluginPath = join(directory, "git-ai.mjs")
    await writeFile(pluginPath, compiled)
    const { default: plugin } = await import(pathToFileURL(pluginPath).href)

    assert.equal(plugin.id, "git-ai")
    assert.equal(typeof plugin.setup, "function")
    assert.equal(typeof plugin.server, "function")

    const hooks = []
    await plugin.setup({
      location: { directory },
      tool: { hook: async (name) => hooks.push(name) },
    })
    assert.deepEqual(hooks, ["execute.before", "execute.after"])

    const legacyHooks = await plugin.server({ directory })
    assert.equal(typeof legacyHooks["tool.execute.before"], "function")
    assert.equal(typeof legacyHooks["tool.execute.after"], "function")
  } finally {
    await rm(directory, { recursive: true, force: true })
  }
})
