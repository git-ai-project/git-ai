import assert from "node:assert/strict"
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { test } from "node:test"
import { pathToFileURL } from "node:url"
import ts from "typescript"
import childProcess from "node:child_process"
import { EventEmitter } from "node:events"
import { syncBuiltinESMExports } from "node:module"

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


// Use actual installed hooks and deterministic child lifecycles; no OpenCode SDK
// or git-ai installation is needed for subprocess/resource boundary tests.
async function fixture(t, childBehavior = (child) => child.emit("close", 0)) {
  const directory = await mkdtemp(join(tmpdir(), "git-ai-opencode-lifecycle-"))
  await mkdir(join(directory, ".git"))
  const source = await readFile(new URL("./git-ai.ts", import.meta.url), "utf8")
  const compiled = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022 },
  }).outputText
  const pluginPath = join(directory, "git-ai.mjs")
  await writeFile(pluginPath, compiled)
  const calls = []
  const children = []
  const warnings = []
  t.mock.method(console, "error", (...args) => warnings.push(args.join(" ")))
  t.mock.method(childProcess, "spawn", () => {
    const child = new EventEmitter()
    child.stderr = new EventEmitter()
    child.stdin = new EventEmitter()
    child.stdin.end = (input) => {
      calls.push(JSON.parse(input))
      queueMicrotask(() => childBehavior(child))
    }
    child.kill = (signal) => { child.signals.push(signal); return true }
    child.signals = []
    children.push(child)
    return child
  })
  syncBuiltinESMExports()
  t.after(async () => {
    t.mock.restoreAll()
    syncBuiltinESMExports()
    await rm(directory, { recursive: true, force: true })
  })
  const { default: plugin } = await import(pathToFileURL(pluginPath).href)
  const hooks = new Map()
  let deliver
  let pending = []
  let subscriptions = 0
  let closed = 0
  const cleanup = await plugin.setup({
    location: { directory },
    tool: { hook: async (name, callback) => hooks.set(name, callback) },
    event: { subscribe: ({ signal }) => ({
      [Symbol.asyncIterator]() {
        subscriptions++
        const close = () => {
          closed++
          deliver?.({ done: true })
          deliver = undefined
          return Promise.resolve({ done: true })
        }
        signal.addEventListener("abort", close, { once: true })
        return {
          next: () => pending.length ? Promise.resolve({ value: pending.shift(), done: false }) : new Promise((resolve) => { deliver = resolve }),
          return: close,
        }
      },
    }) },
  })
  t.after(async () => { await cleanup?.() })
  const emit = async (event) => {
    if (deliver) { const resolve = deliver; deliver = undefined; resolve({ value: event, done: false }) }
    else pending.push(event)
    await new Promise(setImmediate)
  }
  return {
    before: hooks.get("execute.before"), after: hooks.get("execute.after"),
    calls, children, warnings, emit, cleanup,
    subscriptions: () => subscriptions, closed: () => closed,
    event: (id = "call", input = { path: "a.ts" }, sessionID = "session") => ({ id, sessionID, messageID: "message", tool: "edit", input }),
  }
}

for (const type of ["session.tool.failed", "session.tool.success", "session.deleted"]) {
  test(`terminal ${type} releases only the affected pending call/session`, async (t) => {
    const f = await fixture(t)
    const abandoned = f.event("abandoned")
    const healthy = f.event("healthy", { path: "b.ts" }, "other-session")
    await f.before(abandoned)
    await f.before(healthy)
    await f.emit({ type, created: Date.now(), data: { sessionID: abandoned.sessionID, id: abandoned.id, assistantMessageID: abandoned.messageID } })
    await f.after(abandoned)
    await f.after(healthy)
    assert.deepEqual(f.calls.filter((x) => x.hook_event_name === "PostToolUse").map((x) => x.tool_use_id), ["healthy"])
    await f.cleanup?.()
  })
}

test("cancellation during pre checkpoint cannot resurrect a pending call", async (t) => {
  const f = await fixture(t, () => {})
  const event = f.event()
  const pre = f.before(event)
  while (f.children.length === 0) await new Promise(setImmediate)
  await f.emit({ type: "session.tool.failed", created: Date.now(), data: { sessionID: event.sessionID, id: event.id, assistantMessageID: event.messageID } })
  f.children[0].emit("close", 0)
  await pre
  const after = f.after(event)
  await new Promise(setImmediate)
  for (const child of f.children) child.emit("close", 0)
  await after
  assert.equal(f.calls.length, 1, "cancelled reservation must not emit a post checkpoint")
  await f.cleanup?.()
})

test("pending capacity preserves existing long-running calls and resumes after completion", async (t) => {
  const f = await fixture(t)
  for (let i = 0; i < 65; i++) await f.before(f.event(`call-${i}`))
  assert.equal(f.calls.length, 64, "refuse new tracking instead of growing or evicting an active baseline")
  assert.ok(f.warnings.some((line) => /limit|capacity/i.test(line)), "capacity refusal must be visible without debug enabled")
  await f.after(f.event("call-0"))
  await f.before(f.event("replacement"))
  await f.after(f.event("replacement"))
  assert.equal(f.calls.length, 67)
  await f.cleanup?.()
})

test("shell checkpoints retain only bounded canonical command input", async (t) => {
  const f = await fixture(t)
  const event = { ...f.event(), tool: "shell", input: { cmd: "echo hello", unused: "x".repeat(1024 * 1024) } }
  await f.before(event)
  await f.after(event)
  assert.deepEqual(f.calls.map((x) => x.tool_input), [{ command: "echo hello" }, { command: "echo hello" }])
  await f.before({ ...event, id: "oversized", input: { command: "x".repeat(1024 * 1024 + 1) } })
  assert.equal(f.calls.length, 2, "oversized commands must be rejected before pre capture")
  assert.ok(f.warnings.length)
  await f.cleanup?.()
})

test("plugin disposal releases pending state and the event subscription", async (t) => {
  const f = await fixture(t)
  const event = f.event()
  await f.before(event)
  assert.equal(typeof f.cleanup, "function")
  await f.cleanup()
  await f.after(event)
  assert.equal(f.calls.length, 1)
  assert.ok(f.closed() > 0)
})

test("checkpoint stderr is bounded even for a large single chunk", async (t) => {
  const previous = process.env.GIT_AI_OPENCODE_DEBUG
  process.env.GIT_AI_OPENCODE_DEBUG = "1"
  t.after(() => { if (previous === undefined) delete process.env.GIT_AI_OPENCODE_DEBUG; else process.env.GIT_AI_OPENCODE_DEBUG = previous })
  const f = await fixture(t, (child) => {
    child.stderr.emit("data", Buffer.alloc(4 * 1024 * 1024, "x"))
    child.emit("close", 23)
  })
  await f.before(f.event())
  assert.ok(f.warnings.join("").length < 66 * 1024, "stderr retained in the error must be bounded")
  assert.match(f.warnings.join(""), /truncat/i)
})

test("checkpoint timeout escalates to SIGKILL and clears timers on close", async (t) => {
  const f = await fixture(t, () => {})
  t.mock.timers.enable({ apis: ["setTimeout"] })
  const pre = f.before(f.event())
  while (f.children.length === 0) await new Promise(setImmediate)
  t.mock.timers.tick(10_000)
  assert.deepEqual(f.children[0].signals, ["SIGTERM"])
  t.mock.timers.tick(250)
  assert.deepEqual(f.children[0].signals, ["SIGTERM", "SIGKILL"])
  f.children[0].emit("close", null)
  await pre
  t.mock.timers.tick(10_000)
  assert.equal(f.children[0].signals.length, 2)
  await f.cleanup?.()
})


test("a delayed terminal from another assistant message cannot erase a new call", async (t) => {
  const f = await fixture(t)
  const event = f.event()
  await f.before(event)
  await f.emit({ type: "session.tool.failed", data: { sessionID: event.sessionID, id: event.id, assistantMessageID: "older-message" } })
  await f.after(event)
  assert.equal(f.calls.length, 2)
  await f.cleanup?.()
})

test("byte budget bounds accepted shell payloads without evicting an active command", async (t) => {
  const f = await fixture(t)
  const command = "x".repeat(600 * 1024)
  for (let i = 0; i < 20; i++) await f.before({ ...f.event(`large-${i}`), tool: "shell", input: { command } })
  assert.ok(f.calls.length <= 13, "8 MiB total budget must bound accepted command strings")
  await f.after({ ...f.event("large-0"), tool: "shell" })
  assert.equal(f.calls.at(-1).hook_event_name, "PostToolUse")
  assert.equal(f.calls.at(-1).tool_input.command, command)
  await f.cleanup?.()
})

test("cancelled pre calls keep their capacity reservation until their child exits", async (t) => {
  const f = await fixture(t, () => {})
  const pending = []
  for (let i = 0; i < 64; i++) {
    const event = f.event(`slow-${i}`)
    pending.push(f.before(event))
    while (f.children.length <= i) await new Promise(setImmediate)
    await f.emit({ type: "session.tool.failed", data: { sessionID: event.sessionID, id: event.id, assistantMessageID: event.messageID } })
  }
  let extraFinished = false
  const extra = f.before(f.event("extra")).then(() => { extraFinished = true })
  while (!extraFinished && f.children.length === 64) await new Promise(setImmediate)
  const childCount = f.children.length
  for (const child of f.children) child.emit("close", 0)
  await Promise.all([...pending, extra])
  assert.equal(childCount, 64, "cancelled but running children must still consume admission capacity")
  await f.cleanup?.()
})
