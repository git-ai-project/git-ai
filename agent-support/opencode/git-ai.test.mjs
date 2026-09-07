import assert from "node:assert/strict"
import { EventEmitter } from "node:events"
import { mkdtemp, mkdir, rm } from "node:fs/promises"
import { readFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { createRequire } from "node:module"
import { PassThrough } from "node:stream"
import { test } from "node:test"
import { runInNewContext } from "node:vm"
import ts from "typescript"

const require = createRequire(import.meta.url)

async function pluginFixture(t, agent = "codearts", exitCode = 0, subdirectory = "") {
  const directory = await mkdtemp(join(tmpdir(), "git-ai-plugin-"))
  await mkdir(join(directory, ".git"))
  t.after(() => rm(directory, { recursive: true, force: true }))
  const calls = []
  const source = readFileSync(new URL("./git-ai.ts", import.meta.url), "utf8")
    .replace('const AGENT_NAME = "opencode"', `const AGENT_NAME = "${agent}"`)
  const compiled = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  }).outputText
  const exports = {}
  runInNewContext(compiled, {
    exports, process, console, Buffer, setTimeout, clearTimeout,
    require: (name) => name === "child_process" ? {
      spawn(binary, args, options) {
        const child = new EventEmitter()
        child.stderr = new PassThrough()
        child.stdin = new PassThrough()
        let stdin = ""
        child.stdin.on("data", (data) => { stdin += data })
        child.stdin.on("finish", () => {
          calls.push({ binary, args: Array.from(args), options, input: JSON.parse(stdin) })
          queueMicrotask(() => child.emit("close", exitCode))
        })
        child.kill = () => true
        return child
      },
    } : require(name),
  })
  const activeDirectory = join(directory, subdirectory)
  await mkdir(activeDirectory, { recursive: true })
  const hooks = await exports.GitAiPlugin({ directory: activeDirectory, worktree: directory })
  return { directory, calls, hooks }
}

const invocation = (tool, sessionID = "session-1", callID = "call-1") => ({ tool, sessionID, callID })

for (const agent of ["codearts", "opencode"]) {
  test(`${agent}: edit checkpoints preserve call identity and scoped file paths`, async (t) => {
    const { directory, hooks, calls } = await pluginFixture(t, agent)
    const input = invocation("write")
    const args = { filePath: join(directory, "new.ts"), content: "hello" }
    await hooks["tool.execute.before"](input, { args })
    await hooks["tool.execute.after"]({ ...input, args }, { metadata: {} })
    assert.equal(calls.length, 2)
    assert.deepEqual(calls[0].args, ["checkpoint", agent, "--hook-input", "stdin"])
    assert.equal(calls[0].input.hook_event_name, "PreToolUse")
    assert.equal(calls[1].input.hook_event_name, "PostToolUse")
    assert.equal(calls[1].input.tool_use_id, "call-1")
    assert.equal(calls[1].input.session_id, "session-1")
    assert.deepEqual(calls[1].input.tool_input.file_paths, [args.filePath])
    assert.equal(calls[0].options.windowsHide, true)
  })

  test(`${agent}: deleting a session clears its model without changing a pending call`, async (t) => {
    const { hooks, calls } = await pluginFixture(t, agent)
    await hooks["chat.params"]({ sessionID: "deleted", model: { id: "deleted-model" } }, {})
    await hooks["chat.params"]({ sessionID: "retained", model: { id: "retained-model" } }, {})
    const input = invocation("bash", "deleted")
    await hooks["tool.execute.before"](input, { args: { command: "true" } })
    await hooks.event({ event: { type: "session.deleted", properties: { info: { id: "deleted" } } } })
    await hooks["tool.execute.after"](input, {})
    assert.equal(calls.at(-1).input.model, "deleted-model")
    await hooks["tool.execute.before"](invocation("bash", "deleted", "next-call"), { args: { command: "true" } })
    assert.equal(calls.at(-1).input.model, undefined)
    await hooks["tool.execute.before"](invocation("bash", "retained"), { args: { command: "true" } })
    assert.equal(calls.at(-1).input.model, "retained-model")
  })

  test(`${agent}: model cache is bounded and retains recently used sessions`, async (t) => {
    const { hooks, calls } = await pluginFixture(t, agent)
    for (let index = 0; index < 256; index++) {
      await hooks["chat.params"]({ sessionID: `session-${index}`, model: { id: `model-${index}` } }, {})
    }
    const recent = invocation("bash", "session-0")
    await hooks["tool.execute.before"](recent, { args: { command: "true" } })
    await hooks["tool.execute.after"](recent, {})
    await hooks["chat.params"]({ sessionID: "session-256", model: { id: "new-model" } }, {})

    await hooks["tool.execute.before"](invocation("bash", "session-1"), { args: { command: "true" } })
    assert.equal(calls.at(-1).input.model, undefined)
    await hooks["tool.execute.before"](recent, { args: { command: "true" } })
    assert.equal(calls.at(-1).input.model, "model-0")
    await hooks["tool.execute.before"](invocation("bash", "session-256"), { args: { command: "true" } })
    assert.equal(calls.at(-1).input.model, "new-model")
  })

  test(`${agent}: evicting a session model preserves the pending call snapshot`, async (t) => {
    const { hooks, calls } = await pluginFixture(t, agent)
    await hooks["chat.params"]({ sessionID: "pending", model: { id: "original-model" } }, {})
    const input = invocation("bash", "pending")
    await hooks["tool.execute.before"](input, { args: { command: "true" } })
    for (let index = 0; index < 256; index++) {
      await hooks["chat.params"]({ sessionID: `session-${index}`, model: { id: `model-${index}` } }, {})
    }
    await hooks["tool.execute.after"](input, {})
    assert.equal(calls.at(-1).input.model, "original-model")
    await hooks["tool.execute.before"](input, { args: { command: "true" } })
    assert.equal(calls.at(-1).input.model, undefined)
  })
}

test("codearts: model is scoped to each session and shell call", async (t) => {
  const { hooks, calls } = await pluginFixture(t)
  await hooks["chat.params"]({ sessionID: "session-1", model: { id: "deepseek-v3" } }, {})
  await hooks["chat.params"]({ sessionID: "session-2", model: { id: "other-model" } }, {})
  const input = invocation("bash")
  await hooks["tool.execute.before"](input, { args: { command: "echo hello > new.txt" } })
  await hooks["chat.params"]({ sessionID: "session-1", model: { id: "next-model" } }, {})
  await hooks["tool.execute.after"](input, {})
  assert.equal(calls[1].input.model, "deepseek-v3")
  assert.equal(calls[1].input.tool_input.command, "echo hello > new.txt")
})

test("codearts: title generation does not replace the coding model", async (t) => {
  const { hooks, calls } = await pluginFixture(t)
  await hooks["chat.params"]({ sessionID: "session-1", model: { id: "coding-model" } }, {})
  await hooks["chat.params"]({ sessionID: "session-1", model: { id: "title-model" }, isEnsureTitle: true }, {})
  await hooks["chat.params"]({ sessionID: "session-1", model: { id: "cli-title-model" }, agent: "title" }, {})
  await hooks["tool.execute.before"](invocation("bash"), { args: { command: "true" } })
  assert.equal(calls[0].input.model, "coding-model")
})

test("codearts: relative edit paths resolve against tool workdir before checkpointing", async (t) => {
  const { directory, hooks, calls } = await pluginFixture(t)
  await mkdir(join(directory, "src"))
  const input = invocation("edit")
  const args = { workdir: "src", filePath: "main.ts" }
  await hooks["tool.execute.before"](input, { args })
  await hooks["tool.execute.after"]({ ...input, args }, { metadata: { files: [{ filePath: "extra.ts" }] } })
  assert.deepEqual(calls[0].input.tool_input.file_paths, [join(directory, "src", "main.ts")])
  assert.deepEqual(calls[1].input.tool_input.file_paths, [
    join(directory, "src", "main.ts"), join(directory, "src", "extra.ts"),
  ])
})

test("codearts: relative edits use the active directory inside a worktree", async (t) => {
  const { directory, hooks, calls } = await pluginFixture(t, "codearts", 0, "src")
  await hooks["tool.execute.before"](invocation("write"), { args: { filePath: "main.ts" } })
  assert.deepEqual(calls[0].input.tool_input.file_paths, [join(directory, "src", "main.ts")])
})

test("codearts: shell checkpoint cwd preserves the command workdir", async (t) => {
  const { directory, hooks, calls } = await pluginFixture(t)
  await mkdir(join(directory, "src"))
  const input = invocation("bash")
  await hooks["tool.execute.before"](input, { args: { workdir: "src", command: "echo hello > main.ts" } })
  await hooks["tool.execute.after"](input, {})
  assert.equal(calls[0].input.cwd, join(directory, "src"))
  assert.equal(calls[1].input.cwd, join(directory, "src"))
})

test("codearts: patch examples inside file content are not treated as edited paths", async (t) => {
  const { directory, hooks, calls } = await pluginFixture(t)
  await hooks["tool.execute.before"](invocation("write"), { args: {
    filePath: "README.md", content: "*** Update File: unrelated.ts\nfile:///another.ts",
  } })
  assert.deepEqual(calls[0].input.tool_input.file_paths, [join(directory, "README.md")])
})

test("codearts: deleteFile is tracked and read-only tools are skipped", async (t) => {
  const { directory, hooks, calls } = await pluginFixture(t)
  const args = { filePath: join(directory, "old.ts") }
  await hooks["tool.execute.before"](invocation("read"), { args })
  assert.equal(calls.length, 0)
  await hooks["tool.execute.before"](invocation("deleteFile"), { args })
  await hooks["tool.execute.after"](invocation("deleteFile"), {})
  assert.equal(calls.length, 2)
})

test("codearts: missing identifiers and unmatched post events do not checkpoint", async (t) => {
  const { hooks, calls } = await pluginFixture(t)
  await hooks["tool.execute.before"](invocation("bash", "", ""), { args: { command: "true" } })
  await hooks["tool.execute.after"](invocation("bash", "", ""), {})
  await hooks["tool.execute.after"](invocation("bash"), {})
  assert.equal(calls.length, 0)
})

test("codearts: identical call IDs in different sessions stay separate", async (t) => {
  const { hooks, calls } = await pluginFixture(t)
  const first = invocation("bash", "session-1")
  const second = invocation("bash", "session-2")
  await hooks["tool.execute.before"](first, { args: { command: "first" } })
  await hooks["tool.execute.before"](second, { args: { command: "second" } })
  await hooks["tool.execute.after"](first, {})
  await hooks["tool.execute.after"](second, {})
  assert.deepEqual(calls.slice(2).map((call) => [call.input.session_id, call.input.tool_input.command]), [
    ["session-1", "first"], ["session-2", "second"],
  ])
})

test("codearts: checkpoint failures leave tool execution usable", async (t) => {
  const { hooks } = await pluginFixture(t, "codearts", 1)
  const input = invocation("bash")
  await assert.doesNotReject(() => hooks["tool.execute.before"](input, { args: { command: "true" } }))
  await assert.doesNotReject(() => hooks["tool.execute.after"](input, {}))
})
