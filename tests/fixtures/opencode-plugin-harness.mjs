import assert from "node:assert/strict"
import childProcess, { execFile } from "node:child_process"
import { syncBuiltinESMExports } from "node:module"
import { promisify } from "node:util"
import { writeFile } from "node:fs/promises"
import { join } from "node:path"
import { pathToFileURL } from "node:url"

const [pluginPath, repository, api, projectDirectory, scenario, gitAiBinary] = process.argv.slice(2)
const metadataTarget = scenario.startsWith("metadata")
if (scenario === "failed-pre") {
  const spawn = childProcess.spawn
  let first = true
  childProcess.spawn = (command, args, options) => {
    const child = first
      ? spawn(process.execPath, ["-e", "process.exit(23)"], options)
      : spawn(command, args, options)
    first = false
    return child
  }
  syncBuiltinESMExports()
}
const { default: plugin } = await import(pathToFileURL(pluginPath).href)
let before
let after
let terminal
let cleanup
let deliver
let events = []
if (api === "v2") {
  const hooks = new Map()
  cleanup = await plugin.setup({
    location: { directory: repository, project: { directory: projectDirectory } },
    tool: { hook: async (name, callback) => { hooks.set(name, callback) } },
    event: { subscribe: ({ signal }) => ({
      [Symbol.asyncIterator]() {
        signal.addEventListener("abort", () => { deliver?.({ done: true }); deliver = undefined }, { once: true })
        return { next: () => events.length ? Promise.resolve({ value: events.shift(), done: false }) : new Promise((resolve) => { deliver = resolve }) }
      },
    }) },
  })
  assert.equal(hooks.size, 2)
  terminal = async (event) => {
    if (deliver) { const resolve = deliver; deliver = undefined; resolve({ value: event, done: false }) }
    else events.push(event)
    await new Promise(setImmediate)
  }
  before = (event) => hooks.get("execute.before")(event)
  after = (event) => hooks.get("execute.after")(event)
} else {
  const hooks = await plugin.server({ worktree: repository, directory: process.cwd() })
  terminal = async (event) => hooks.event?.({ event })
  before = (event) => hooks["tool.execute.before"]({
    tool: event.tool, sessionID: event.sessionID, callID: event.id,
  }, { args: event.input })
  after = (event) => hooks["tool.execute.after"]({
    tool: event.tool, sessionID: event.sessionID, callID: event.id, args: event.input,
  }, { metadata: event.result.metadata })
}

const first = {
  id: "call-first", messageID: "message-first", sessionID: "test-session-123", tool: "edit",
  input: api === "v2" ? { path: "first.ts" } : { filePath: "first.ts" },
}
const second = {
  id: "call-second", messageID: "message-second", sessionID: "test-session-123", tool: metadataTarget ? "patch" : "write",
  input: metadataTarget ? { patchText: "@@ -1 +1 @@\n-old\n+new" } : api === "v2" ? { path: "second.ts" } : { filePath: "second.ts" },
}
if (scenario === "large-input") {
  // A patch can contain a very large body but only a few file headers. Path
  // discovery must not allocate an array/string object for every body line.
  first.tool = "patch"
  first.input = { patchText: "not a patch line\n".repeat(2_000_000)
    + "\t*** Update File: 'first.ts'\r\n" }
}
if (scenario === "sliced-input") {
  // Short absolute paths can be slices backed by an entire patch body. Keep
  // several calls pending under the heap limit to exercise retained storage.
  for (let i = 0; i < 8; i++) {
    await before({ ...first, id: `pending-${i}`, tool: "patch", input: {
      patchText: "not a patch line\n".repeat(1_000_000)
        + `*** Update File: ${join(repository, "first.ts")}\n`,
    } })
    await new Promise(setImmediate)
    global.gc()
  }
}
await before(first)
await before(second)
if (scenario.startsWith("lifecycle")) {
  await terminal(api === "v2" ? {
    type: "session.tool.failed", data: {
      sessionID: first.sessionID, assistantMessageID: first.messageID, id: first.id,
      error: { name: scenario === "lifecycle-decline" ? "PermissionDeniedError" : "AbortError", message: "Stopped" },
    },
  } : {
    type: "message.part.updated", properties: { part: {
      type: "tool", sessionID: first.sessionID, callID: first.id, state: { status: "error" },
    } },
  })
}
await writeFile(join(repository, "first.ts"), "// baseline\n// manual first\n// AI first\n")
await writeFile(join(repository, "second.ts"), "// baseline\n// manual second\n// AI second\n")
await writeFile(join(repository, "unrelated.ts"), "// baseline\n// unrelated manual\n")
// Completions can arrive in a different order from their before hooks.
await after({ ...second, status: "completed", result: {
  metadata: scenario === "metadata-extra" ? {} : { files: [{ file: "second.ts" }] },
} })
await after({ ...first, status: "completed", result: {
  metadata: scenario === "metadata-extra" ? { files: [{ file: "second.ts" }] } : {},
} })
// An unmatched or duplicate completion must not claim a subsequent manual edit.
await writeFile(join(repository, "first.ts"), "// baseline\n// manual first\n// AI first\n// manual after\n")
await after({ ...first, status: "completed", result: { metadata: {} } })

if (scenario !== "untracked" && scenario !== "failed-pre") {
  await promisify(execFile)(gitAiBinary, ["checkpoint", "mock_known_human", join(repository, "first.ts")])
}

await cleanup?.()
