/**
 * git-ai plugin for OpenCode
 *
 * This plugin integrates git-ai with OpenCode to track AI-generated code.
 * It uses the tool execute before/after hooks to create checkpoints that mark
 * code changes as human or AI-authored.
 *
 * Supports both plugin APIs from a single default export:
 *   - OpenCode V2 (`@opencode/plugin`): `setup(ctx)` registers the hooks through
 *     `ctx.tool.hook("execute.before" | "execute.after", ...)`.
 *   - OpenCode V1 (`@opencode-ai/plugin`, >= 1.18.29): `server()` returns the
 *     legacy `"tool.execute.before" | "tool.execute.after"` hook object.
 *
 * Installation:
 *   - Automatically installed by `git-ai install-hooks`
 *   - Or manually copy to ~/.config/opencode/plugins/git-ai.ts (global)
 *   - Or to .opencode/plugins/git-ai.ts (project-local)
 *
 * Requirements:
 *   - git-ai must be installed (path is injected at install time)
 *
 * @see https://github.com/git-ai-project/git-ai
 * @see https://opencode.ai/v2/docs/build/plugins
 */

import type { Plugin } from "@opencode/plugin"
import { spawn } from "child_process"
import { readFile, stat } from "fs/promises"
import { dirname, isAbsolute, join, resolve } from "path"

// Absolute path to git-ai binary, replaced at install time by `git-ai install-hooks`
const GIT_AI_BIN = "__GIT_AI_BINARY_PATH__"
const CHECKPOINT_TIMEOUT_MS = 10_000
const CHECKPOINT_KILL_GRACE_MS = 250
const MAX_STDERR_BYTES = 64 * 1024
const MAX_PENDING_CALLS = 64
const MAX_PENDING_BYTES = 8 * 1024 * 1024
const MAX_CALL_BYTES = 1024 * 1024
const CHECKPOINT_ARGS = ["checkpoint", "opencode", "--hook-input", "stdin"]

// Tools that modify files and should be tracked
const FILE_EDIT_TOOLS = new Set([
  "edit",
  "write",
  "patch",
  "multiedit",
  "apply_patch",
  "applypatch",
])

const APPLY_PATCH_FILE_PREFIXES = [
  "*** Update File: ",
  "*** Add File: ",
  "*** Delete File: ",
  "*** Move to: ",
]

const isEditTool = (toolName: string): boolean => FILE_EDIT_TOOLS.has(toolName.toLowerCase())

const isBashTool = (toolName: string): boolean => {
  const name = toolName.toLowerCase()
  return name === "bash" || name === "shell"
}

const normalizePath = (rawPath: string, cwd?: string): string | null => {
  const trimmed = rawPath.trim().replace(/^['"]|['"]$/g, "")
  if (!trimmed) {
    return null
  }

  const withoutScheme = trimmed
    .replace(/^file:\/\/localhost/, "")
    .replace(/^file:\/\//, "")

  const isWindowsAbs = /^[a-zA-Z]:[\\/]/.test(withoutScheme)
  if (isAbsolute(withoutScheme) || isWindowsAbs) {
    return withoutScheme
  }

  // Use provided cwd, or fall back to process.cwd() for relative paths
  const resolvedCwd = cwd || process.cwd()
  return join(resolvedCwd, withoutScheme)
}

const collectApplyPatchPaths = (raw: string, out: Set<string>): void => {
  // Scan header candidates only. Splitting a large edit/patch body creates a
  // separate array entry and string for every line even when it has no paths.
  let offset = raw.indexOf("*** ")
  while (offset !== -1) {
    const lineStart = raw.lastIndexOf("\n", offset - 1) + 1
    const newline = raw.indexOf("\n", offset)
    const lineEnd = newline === -1 ? raw.length : newline
    if (raw.slice(lineStart, offset).trim() === "") {
      const header = raw.slice(offset, lineEnd).trim()
      for (const prefix of APPLY_PATCH_FILE_PREFIXES) {
        if (header.startsWith(prefix)) {
          const path = header.slice(prefix.length).trim().replace(/^['"]|['"]$/g, "")
          if (path) {
            out.add(path)
          }
        }
      }
    }
    offset = newline === -1 ? -1 : raw.indexOf("*** ", newline + 1)
  }
}

const collectToolPaths = (value: unknown, out: Set<string>): void => {
  if (typeof value === "string") {
    if (value.startsWith("file://")) {
      out.add(value)
    }
    collectApplyPatchPaths(value, out)
    return
  }

  if (Array.isArray(value)) {
    for (const item of value) {
      collectToolPaths(item, out)
    }
    return
  }

  if (!value || typeof value !== "object") {
    return
  }

  for (const [key, val] of Object.entries(value)) {
    const keyLower = key.toLowerCase()
    const isSinglePathKey = keyLower === "file_path" || keyLower === "filepath" || keyLower === "path" || keyLower === "fspath"
    const isMultiPathKey = keyLower === "files" || keyLower === "filepaths" || keyLower === "file_paths"

    if (isSinglePathKey && typeof val === "string") {
      out.add(val)
    } else if (isMultiPathKey) {
      if (typeof val === "string") {
        out.add(val)
      } else if (Array.isArray(val)) {
        for (const item of val) {
          if (typeof item === "string") {
            out.add(item)
          }
        }
      }
    }

    collectToolPaths(val, out)
  }
}

const extractFilePaths = (args: unknown, cwd?: string): string[] => {
  const rawPaths = new Set<string>()
  collectToolPaths(args, rawPaths)

  const normalizedPaths = new Set<string>()
  for (const rawPath of rawPaths) {
    const normalized = normalizePath(rawPath, cwd)
    if (normalized) {
      normalizedPaths.add(normalized)
    }
  }

  return [...normalizedPaths]
}

/**
 * Normalized checkpoint event used by the shared core. Both plugin APIs map
 * their own hook payload into this shape.
 */
type CheckpointEvent = {
  tool?: unknown
  sessionID?: unknown
  callID?: unknown
  messageID?: unknown
  toolInput?: unknown
}

const asRecord = (value: unknown): Record<string, unknown> | undefined => {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    return undefined
  }

  return value as Record<string, unknown>
}

const hookString = (value: unknown): string => typeof value === "string" ? value : ""

const extractToolCwd = (args: Record<string, unknown> | undefined): string | undefined => {
  if (typeof args?.workdir === "string") return args.workdir
  if (typeof args?.cwd === "string") return args.cwd
  return undefined
}

const debugEnabled = (): boolean => {
  const value = process.env.GIT_AI_OPENCODE_DEBUG ?? process.env.GIT_AI_DEBUG
  return value === "1" || value?.toLowerCase() === "true"
}

const debugLog = (message: string, error?: unknown): void => {
  if (!debugEnabled()) {
    return
  }

  try {
    const detail = error instanceof Error
      ? `${error.name}: ${error.message}`
      : error === undefined
        ? ""
        : String(error)
    console.error(`[git-ai opencode] ${message}${detail ? `: ${detail}` : ""}`)
  } catch {
    // Debug logging must never be the reason a hook fails.
  }
}

const swallowHookErrors = <Args extends unknown[]>(
  label: string,
  hook: (...args: Args) => Promise<void>,
): ((...args: Args) => Promise<void>) => {
  return async (...args) => {
    try {
      await hook(...args)
    } catch (error) {
      debugLog(label, error)
    }
  }
}

const runCheckpoint = (hookInput: string): Promise<void> => {
  return new Promise((resolve, reject) => {
    let timeout: ReturnType<typeof setTimeout> | undefined
    let killTimeout: ReturnType<typeof setTimeout> | undefined
    let timeoutError: Error | undefined
    let closed = false
    let exited = false
    let stderr: Buffer | undefined
    let stderrBytes = 0
    let stderrTruncated = false
    const finish = (error: Error | null): void => {
      if (closed) return
      closed = true
      clearTimeout(timeout)
      clearTimeout(killTimeout)
      stderr = undefined
      if (error) reject(error)
      else resolve()
    }

    const child = spawn(GIT_AI_BIN, CHECKPOINT_ARGS, {
      stdio: ["pipe", "ignore", "pipe"],
    })
    const closeTimedOut = (): void => {
      // A descendant can inherit stderr after the checkpoint itself has exited.
      // Close only our pipes; never signal an unrelated process group or daemon.
      child.stdin.destroy()
      child.stderr.destroy()
      finish(timeoutError ?? null)
    }
    timeout = setTimeout(() => {
      timeoutError = new Error(`git-ai checkpoint opencode timed out after ${CHECKPOINT_TIMEOUT_MS}ms`)
      if (exited) return closeTimedOut()
      child.kill("SIGTERM")
      if (!closed) {
        killTimeout = setTimeout(() => {
          child.kill("SIGKILL")
          closeTimedOut()
        }, CHECKPOINT_KILL_GRACE_MS)
      }
    }, CHECKPOINT_TIMEOUT_MS)

    child.stderr.on("data", (chunk: Buffer) => {
      if (closed) return
      const remaining = MAX_STDERR_BYTES - stderrBytes
      const length = Math.min(chunk.length, remaining)
      if (length > 0) {
        stderr ??= Buffer.allocUnsafe(MAX_STDERR_BYTES)
        chunk.copy(stderr, stderrBytes, 0, length)
        stderrBytes += length
      }
      stderrTruncated ||= chunk.length > remaining
    })
    child.stderr.on("error", (error) => debugLog("failed to read checkpoint stderr", error))
    child.stdin.on("error", () => {
      // Early exit/EPIPE is reported through close/error, without an unhandled event.
    })
    child.on("exit", () => {
      exited = true
      if (timeoutError) closeTimedOut()
    })
    child.on("error", finish)
    child.on("close", (code) => {
      const detail = stderr?.toString("utf8", 0, stderrBytes).trim() ?? ""
      finish(timeoutError ?? (code === 0 ? null : new Error(
        `git-ai checkpoint opencode exited with ${code}${detail ? `: ${detail}` : ""}${stderrTruncated ? " [stderr truncated]" : ""}`,
      )))
    })
    child.stdin.end(hookInput)
  })
}

type PendingCall = {
  key: string
  sessionID: string
  callID: string
  messageID: string
  toolName: string
  toolCwd: string
  filePaths: string[]
  toolInput: { file_paths: string[] } | { command?: string }
  bytes: number
  repoDir?: string
  phase: "pre" | "ready" | "post"
  cancelled: boolean
}

// V8 can represent a short string as a slice retaining a much larger input.
// Copy only admitted canonical values; UTF-16 preserves every JS code unit.
const ownString = (value: string): string => Buffer.from(value, "utf16le").toString("utf16le")

const createGitAiCheckpoints = (defaultCwd: string, onPendingChange: (active: boolean) => void = () => {}) => {
  const pendingCalls = new Map<string, PendingCall>()
  let pendingBytes = 0
  let warned = false
  let disposed = false
  const warnLimit = (): void => {
    if (!warned) {
      warned = true
      console.error("[git-ai opencode] Checkpoint capacity limit reached; this tool call will not be attributed. Existing calls remain tracked.")
    }
  }
  const release = (call: PendingCall): void => {
    if (pendingCalls.get(call.key) !== call) return
    pendingCalls.delete(call.key)
    pendingBytes -= call.bytes
    warned = false
    onPendingChange(!disposed && pendingCalls.size > 0)
  }
  const cancel = (call: PendingCall): void => {
    call.cancelled = true
    // In-flight pre/post checkpoints still own their reservation until close.
    if (call.phase === "ready") release(call)
  }
  const terminal = (sessionID: string, callID?: string, messageID?: string): void => {
    if (callID !== undefined) {
      const call = pendingCalls.get(JSON.stringify([sessionID, callID]))
      if (call && (messageID === undefined || call.messageID === messageID)) cancel(call)
    } else {
      for (const call of pendingCalls.values()) if (call.sessionID === sessionID) cancel(call)
    }
  }
  const dispose = (): void => {
    disposed = true
    for (const call of pendingCalls.values()) cancel(call)
  }

  const nearestExistingDirectory = async (pathHint: string): Promise<string | null> => {
    let candidate = pathHint
    while (candidate) {
      try {
        const fileStat = await stat(candidate)
        return fileStat.isDirectory() ? candidate : dirname(candidate)
      } catch (error) {
        debugLog(`failed to stat path while resolving git repo from ${candidate}`, error)
      }

      const parent = dirname(candidate)
      if (parent === candidate) {
        break
      }
      candidate = parent
    }

    return null
  }

  const isGitDirPointer = async (gitFilePath: string, worktreeDir: string): Promise<boolean> => {
    try {
      const firstLine = (await readFile(gitFilePath, "utf8")).split(/\r?\n/, 1)[0]?.trim() ?? ""
      if (!firstLine.toLowerCase().startsWith("gitdir:")) {
        return false
      }

      const gitDir = firstLine.slice("gitdir:".length).trim()
      if (!gitDir) {
        return false
      }

      const gitDirPath = isAbsolute(gitDir) || /^[a-zA-Z]:[\\/]/.test(gitDir)
        ? gitDir
        : resolve(worktreeDir, gitDir)
      return (await stat(gitDirPath)).isDirectory()
    } catch (error) {
      debugLog(`failed to read gitdir pointer from ${gitFilePath}`, error)
      return false
    }
  }

  const hasGitMetadata = async (dir: string): Promise<boolean> => {
    const marker = join(dir, ".git")
    try {
      const fileStat = await stat(marker)
      if (fileStat.isDirectory()) {
        return true
      }

      if (fileStat.isFile()) {
        return await isGitDirPointer(marker, dir)
      }
    } catch (error) {
      debugLog(`failed to inspect git metadata at ${marker}`, error)
    }

    return false
  }

  // Helper to find git repo root from a file path or directory
  const findGitRepo = async (pathHint: string): Promise<string | null> => {
    let dir = await nearestExistingDirectory(pathHint)
    while (dir) {
      if (await hasGitMetadata(dir)) {
        return dir
      }

      const parent = dirname(dir)
      if (parent === dir) {
        break
      }
      dir = parent
    }

    return null
  }

  const resolveCwd = (cwd?: string): string => {
    if (!cwd) {
      return defaultCwd
    }

    return normalizePath(cwd, defaultCwd) || defaultCwd
  }

  const resolveRepoDir = async (filePaths: string[], cwd?: string): Promise<string | null> => {
    const seenHints = new Set<string>()
    const findGitRepoOnce = async (pathHint: string | undefined): Promise<string | null> => {
      if (!pathHint || seenHints.has(pathHint)) {
        return null
      }

      seenHints.add(pathHint)
      return await findGitRepo(pathHint)
    }

    for (const filePath of filePaths) {
      const repo = await findGitRepoOnce(filePath)
      if (repo) {
        return repo
      }
    }

    const fromCwd = await findGitRepoOnce(cwd)
    if (fromCwd) {
      return fromCwd
    }

    const fromDefaultCwd = await findGitRepoOnce(defaultCwd)
    if (fromDefaultCwd) {
      return fromDefaultCwd
    }

    const fromProcessCwd = await findGitRepoOnce(process.cwd())
    if (fromProcessCwd) {
      return fromProcessCwd
    }

    return null
  }

  const captureBefore = swallowHookErrors("pre-tool checkpoint failed", async (call: PendingCall) => {
    try {
      const repoDir = await resolveRepoDir(call.filePaths, call.toolCwd)
      if (!repoDir || call.cancelled) return
      call.repoDir = repoDir
      await runCheckpoint(JSON.stringify({
        hook_event_name: "PreToolUse", session_id: call.sessionID, tool_use_id: call.callID,
        cwd: repoDir, tool_name: call.toolName, tool_input: call.toolInput,
      }))
      if (!call.cancelled) call.phase = "ready"
    } finally {
      if (call.phase !== "ready") release(call)
    }
  })

  // Canonicalize synchronously before entering an async function, so suspended
  // checkpoints do not retain the original tool event, file contents or metadata.
  const prepareBefore = (event: CheckpointEvent): Promise<void> => {
    if (disposed) return Promise.resolve()
    const toolName = hookString(event.tool)
    const edit = isEditTool(toolName)
    if (!edit && !isBashTool(toolName)) return Promise.resolve()
    const sessionID = hookString(event.sessionID)
    const callID = hookString(event.callID)
    const key = JSON.stringify([sessionID, callID])
    const previous = pendingCalls.get(key)
    if (previous) {
      cancel(previous)
      if (previous.phase !== "ready") return Promise.resolve()
    }
    if (pendingCalls.size >= MAX_PENDING_CALLS) {
      warnLimit()
      return Promise.resolve()
    }
    const toolCwd = resolveCwd(extractToolCwd(asRecord(event.toolInput)))
    const filePaths = edit ? extractFilePaths(event.toolInput, toolCwd) : []
    const input = asRecord(event.toolInput)
    const command = hookString(input?.command ?? input?.cmd)
    if (Buffer.byteLength(command) > MAX_CALL_BYTES) {
      warnLimit()
      return Promise.resolve()
    }
    const toolInput = edit ? { file_paths: filePaths } : { command }
    const bytes = Buffer.byteLength(JSON.stringify(toolInput)) + Buffer.byteLength(key + toolCwd + hookString(event.messageID))
    if (bytes > MAX_CALL_BYTES || pendingBytes + bytes > MAX_PENDING_BYTES) {
      warnLimit()
      return Promise.resolve()
    }
    const ownedPaths = filePaths.map(ownString)
    const call: PendingCall = {
      key, sessionID: ownString(sessionID), callID: ownString(callID),
      messageID: ownString(hookString(event.messageID)), toolName: ownString(toolName),
      toolCwd: ownString(toolCwd), filePaths: ownedPaths,
      toolInput: edit ? { file_paths: ownedPaths } : { command: ownString(command) },
      bytes, phase: "pre", cancelled: false,
    }
    pendingCalls.set(key, call)
    pendingBytes += bytes
    onPendingChange(true)
    return captureBefore(call)
  }

  const before = (event: CheckpointEvent): Promise<void> => {
    try { return prepareBefore(event) }
    catch (error) {
      debugLog("pre-tool checkpoint failed", error)
      return Promise.resolve()
    }
  }

  const after = swallowHookErrors("post-tool checkpoint failed", async (event: CheckpointEvent) => {
    const key = JSON.stringify([hookString(event.sessionID), hookString(event.callID)])
    const call = pendingCalls.get(key)
    if (!call || call.phase !== "ready" || call.cancelled || call.messageID !== hookString(event.messageID)) return
    call.phase = "post"
    try {
      await runCheckpoint(JSON.stringify({
        hook_event_name: "PostToolUse", session_id: call.sessionID, tool_use_id: call.callID,
        cwd: call.repoDir, tool_name: call.toolName, tool_input: call.toolInput,
      }))
    } finally {
      release(call)
    }
  })

  return { before, after, terminal, dispose }
}

/**
 * OpenCode V2 plugin definition (`@opencode/plugin`).
 *
 * Hooks are registered on the tool domain; the callback receives a single event
 * (`id`, `input`, `tool`, `sessionID`, and `status`/`result` after execution).
 */
export const GitAiPlugin: Plugin.Plugin = {
  id: "git-ai",
  async setup(ctx) {
    try {
      const defaultCwd =
        ctx.location?.directory ?? ctx.location?.project?.directory ?? process.cwd()
      let controller: AbortController | undefined
      const { before, after, terminal, dispose } = createGitAiCheckpoints(defaultCwd, (active) => {
        if (!active) {
          controller?.abort()
          controller = undefined
          return
        }
        if (controller || !ctx.event?.subscribe) return
        const current = new AbortController()
        controller = current
        // 2.2 filters before encoding; 2.1 supports signal but ignores types.
        // Subscribe only while calls are pending, and do no async work per event.
        void (async () => {
          const options = {
            signal: current.signal,
            types: ["session.tool.success", "session.tool.failed", "session.deleted"] as const,
          }
          for await (const event of ctx.event.subscribe(options)) {
            if (current.signal.aborted) break
            const data = asRecord(event.data)
            const sessionID = hookString(data?.sessionID)
            if (event.type === "session.deleted") terminal(sessionID)
            else if (event.type === "session.tool.success" || event.type === "session.tool.failed") {
              terminal(sessionID, hookString(data?.id), hookString(data?.assistantMessageID))
            }
          }
        })().catch((error) => debugLog("lifecycle subscription failed", error)).finally(() => {
          if (controller === current) controller = undefined
        })
      })

      await ctx.tool.hook("execute.before", (event) => before({
        tool: event.tool, sessionID: event.sessionID, callID: event.id,
        messageID: event.messageID, toolInput: event.input,
      }))
      await ctx.tool.hook("execute.after", (event) => after({
        tool: event.tool, sessionID: event.sessionID, callID: event.id, messageID: event.messageID,
      }))
      return () => {
        controller?.abort()
        controller = undefined
        dispose()
      }
    } catch (error) {
      debugLog("failed to initialize plugin", error)
    }
  },
}

/**
 * OpenCode V1 plugin implementation (`@opencode-ai/plugin`).
 *
 * V1 calls `server(ctx)` and expects the returned hook object. Kept so the same
 * package keeps working on OpenCode 1.18.x while V2 support is live.
 */
type V1PluginContext = {
  worktree?: string
  directory?: string
}

type V1ToolHookInput = {
  tool?: unknown
  sessionID?: unknown
  callID?: unknown
  args?: unknown
}

export const GitAiPluginV1 = (ctx?: V1PluginContext) => {
  const defaultCwd = ctx?.worktree || ctx?.directory || process.cwd()
  const { before, after, terminal } = createGitAiCheckpoints(defaultCwd)

  return {
    event: async ({ event }: { event: { type?: string; properties?: Record<string, unknown> } }) => {
      const properties = event.properties
      if (event.type === "session.deleted") terminal(hookString(asRecord(properties?.info)?.id))
      if (event.type === "message.part.updated") {
        const part = asRecord(properties?.part)
        const state = asRecord(part?.state)
        if (part?.type === "tool" && (state?.status === "completed" || state?.status === "error")) {
          terminal(hookString(part.sessionID), hookString(part.callID))
        }
      }
    },
    "tool.execute.before": (input: V1ToolHookInput, output?: { args?: unknown }) =>
      before({
        tool: input.tool,
        sessionID: input.sessionID,
        callID: input.callID,
        toolInput: output?.args ?? input.args,
      }),
    "tool.execute.after": (input: V1ToolHookInput) =>
      after({
        tool: input.tool,
        sessionID: input.sessionID,
        callID: input.callID,
      }),
  }
}

export default {
  ...GitAiPlugin,
  async server(ctx?: V1PluginContext) {
    return GitAiPluginV1(ctx)
  },
}
