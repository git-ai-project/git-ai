import assert from "node:assert/strict"
import { execFile } from "node:child_process"
import { promisify } from "node:util"
import { writeFile } from "node:fs/promises"
import { join } from "node:path"
import { pathToFileURL } from "node:url"

const [pluginPath, repository, api, projectDirectory, scenario, gitAiBinary] = process.argv.slice(2)
const { default: plugin } = await import(pathToFileURL(pluginPath).href)
let before
let after
if (api === "v2") {
  const hooks = new Map()
  await plugin.setup({
    location: { directory: repository, project: { directory: projectDirectory } },
    tool: { hook: async (name, callback) => { hooks.set(name, callback) } },
  })
  assert.equal(hooks.size, 2)
  before = (event) => hooks.get("execute.before")(event)
  after = (event) => hooks.get("execute.after")(event)
} else {
  const hooks = await plugin.server({ worktree: repository, directory: process.cwd() })
  before = (event) => hooks["tool.execute.before"]({
    tool: event.tool, sessionID: event.sessionID, callID: event.id,
  }, { args: event.input })
  after = (event) => hooks["tool.execute.after"]({
    tool: event.tool, sessionID: event.sessionID, callID: event.id, args: event.input,
  }, { metadata: event.result.metadata })
}

const first = {
  id: "call-first", sessionID: "test-session-123", tool: "edit",
  input: api === "v2" ? { path: "first.ts" } : { filePath: "first.ts" },
}
const second = {
  id: "call-second", sessionID: "test-session-123", tool: scenario === "metadata" ? "patch" : "write",
  input: scenario === "metadata" ? { patchText: "@@ -1 +1 @@\n-old\n+new" } : api === "v2" ? { path: "second.ts" } : { filePath: "second.ts" },
}
await before(first)
await before(second)
await writeFile(join(repository, "first.ts"), "// baseline\n// manual first\n// AI first\n")
await writeFile(join(repository, "second.ts"), scenario === "metadata"
  ? "// baseline\n// AI second\n"
  : "// baseline\n// manual second\n// AI second\n")
await writeFile(join(repository, "unrelated.ts"), "// baseline\n// unrelated manual\n")
// Completions can arrive in a different order from their before hooks.
await after({ ...second, status: "completed", result: { metadata: { files: [{ file: "second.ts" }] } } })
await after({ ...first, status: "completed", result: { metadata: {} } })
// An unmatched or duplicate completion must not claim a subsequent manual edit.
await writeFile(join(repository, "first.ts"), "// baseline\n// manual first\n// AI first\n// manual after\n")
await after({ ...first, status: "completed", result: { metadata: {} } })

if (scenario !== "untracked") {
  await promisify(execFile)(gitAiBinary, ["checkpoint", "mock_known_human", join(repository, "first.ts")])
}
