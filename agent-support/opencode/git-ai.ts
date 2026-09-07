/**
 * git-ai plugin for OpenCode
 *
 * This plugin integrates git-ai with OpenCode to track AI-generated code.
 * It uses the tool.execute.before and tool.execute.after events to create
 * checkpoints that mark code changes as human or AI-authored.
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
 * @see https://opencode.ai/docs/plugins/
 */

import type { Hooks, Plugin } from "@opencode-ai/plugin"
import { spawn } from "child_process"
import { readFile, stat } from "fs/promises"
import { dirname, isAbsolute, join, resolve } from "path"
import { fileURLToPath } from "url"

// Also embedded by the CodeArts installer, which substitutes this agent name.
const AGENT_NAME = "opencode"
// Absolute path to git-ai binary, replaced at install time by `git-ai install-hooks`
const GIT_AI_BIN = "__GIT_AI_BINARY_PATH__"
const CHECKPOINT_TIMEOUT_MS = 10_000
const CHECKPOINT_ARGS = ["checkpoint", AGENT_NAME, "--hook-input", "stdin"]
const MAX_SESSION_MODELS = 256
const MAX_PENDING_CALLS = 1024

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

const isEditTool = (toolName: string): boolean =>
  FILE_EDIT_TOOLS.has(toolName.toLowerCase()) ||
  (AGENT_NAME.toString() === "codearts" && toolName.toLowerCase() === "deletefile")

const isBashTool = (toolName: string): boolean => {
  const name = toolName.toLowerCase()
  return name === "bash" || name === "shell"
}

const normalizePath = (rawPath: string, cwd?: string, toolName?: string): string | null => {
  const trimmed = rawPath.trim().replace(/^['"]|['"]$/g, "")
  if (!trimmed) {
    return null
  }

  if (trimmed.startsWith("file://")) {
    try {
      return fileURLToPath(trimmed)
    } catch {
      return null
    }
  }

  // CodeArts 26.8.1 rewrites MSYS drive paths before edit/deleteFile execute.
  // Its write and patch tools do not apply that execution-time transformation.
  const isCodeArtsDrivePath = AGENT_NAME.toString() === "codearts" && process.platform === "win32"
    && (toolName?.toLowerCase() === "edit" || toolName?.toLowerCase() === "deletefile")
  const toolPath = isCodeArtsDrivePath ? trimmed.replace(/^\/([a-zA-Z])\//, "$1:/") : trimmed

  const isWindowsAbs = /^[a-zA-Z]:[\\/]/.test(toolPath)
  if (isAbsolute(toolPath) || isWindowsAbs) {
    return toolPath
  }

  // Use provided cwd, or fall back to process.cwd() for relative paths
  const resolvedCwd = cwd || process.cwd()
  return join(resolvedCwd, toolPath)
}

const collectApplyPatchPaths = (raw: string, out: Set<string>): void => {
  for (const line of raw.split("\n")) {
    const trimmed = line.trim()
    for (const prefix of APPLY_PATCH_FILE_PREFIXES) {
      if (trimmed.startsWith(prefix)) {
        const path = trimmed.slice(prefix.length).trim().replace(/^['"]|['"]$/g, "")
        if (path) {
          out.add(path)
        }
      }
    }
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
    // Source text may itself contain file URIs or examples of apply_patch input.
    if (["content", "oldstring", "newstring", "oldtext", "newtext", "old_text", "new_text"].includes(keyLower)) {
      continue
    }
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

const extractFilePaths = (args: unknown, cwd?: string, toolName?: string): string[] => {
  const rawPaths = new Set<string>()
  collectToolPaths(args, rawPaths)

  const normalizedPaths = new Set<string>()
  for (const rawPath of rawPaths) {
    const normalized = normalizePath(rawPath, cwd, toolName)
    if (normalized) {
      normalizedPaths.add(normalized)
    }
  }

  return [...normalizedPaths]
}

type ToolHookInput = {
  tool?: unknown
  sessionID?: unknown
  callID?: unknown
  args?: unknown
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
  const value = process.env[`GIT_AI_${AGENT_NAME.toUpperCase()}_DEBUG`] ?? process.env.GIT_AI_DEBUG
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
    console.error(`[git-ai ${AGENT_NAME}] ${message}${detail ? `: ${detail}` : ""}`)
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
    let settled = false
    let timeout: ReturnType<typeof setTimeout> | undefined
    const finish = (error: Error | null): void => {
      if (settled) {
        return
      }

      settled = true
      if (timeout) {
        clearTimeout(timeout)
      }
      if (error) {
        reject(error)
      } else {
        resolve()
      }
    }

    const child = spawn(GIT_AI_BIN, CHECKPOINT_ARGS, {
      stdio: ["pipe", "ignore", "pipe"],
      windowsHide: true,
    })

    timeout = setTimeout(() => {
      try {
        child.kill("SIGTERM")
      } catch (error) {
        debugLog("failed to kill timed-out checkpoint command", error)
      }
      finish(new Error(`git-ai checkpoint ${AGENT_NAME} timed out after ${CHECKPOINT_TIMEOUT_MS}ms`))
    }, CHECKPOINT_TIMEOUT_MS)

    const stderr: Buffer[] = []

    child.stderr.on("data", (chunk: Buffer) => stderr.push(chunk))
    child.stderr.on("error", (error) => {
      debugLog("failed to read checkpoint stderr", error)
    })
    child.stdin.on("error", () => {
      // The child may exit before stdin is fully written; close/error handling below reports failures.
    })
    child.on("error", finish)
    child.on("close", (code) => {
      if (code === 0) {
        finish(null)
        return
      }

      const stderrText = Buffer.concat(stderr).toString().trim()
      finish(new Error(`git-ai checkpoint ${AGENT_NAME} exited with ${code}${stderrText ? `: ${stderrText}` : ""}`))
    })

    child.stdin.end(hookInput)
  })
}

export const GitAiPlugin: Plugin = async (ctx) => {
  try {
    return createGitAiPlugin(ctx)
  } catch (error) {
    debugLog("failed to initialize plugin", error)
    return {}
  }
}

const createGitAiPlugin = (ctx: Parameters<Plugin>[0]): Awaited<ReturnType<Plugin>> => {
  const { worktree, directory } = ctx
  const defaultCwd = directory || worktree || process.cwd()

  // Call IDs can be reused across sessions. Keep each before/after pair together.
  const callKey = (sessionID: string, callID: string): string => JSON.stringify([sessionID, callID])
  const pendingCalls = new Map<string, { cwd: string; sessionID: string; toolInput: unknown; model?: string }>()
  const sessionModels = new Map<string, string>()
  const rememberSessionModel = (sessionID: string, model: string): void => {
    // Keep recent sessions even when the client never emits session.deleted.
    sessionModels.delete(sessionID)
    sessionModels.set(sessionID, model)
    if (sessionModels.size > MAX_SESSION_MODELS) {
      const oldestSessionID = sessionModels.keys().next().value
      if (oldestSessionID !== undefined) sessionModels.delete(oldestSessionID)
    }
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

  return {
    event: swallowHookErrors(
      "tool/session cleanup failed",
      async ({ event }: Parameters<NonNullable<Hooks["event"]>>[0]) => {
        if (event.type === "session.deleted") {
          sessionModels.delete(event.properties.info.id)
        }
        if (event.type === "message.part.updated") {
          const part = event.properties.part
          if (part.type === "tool" && part.state.status === "error") {
            pendingCalls.delete(callKey(part.sessionID, part.callID))
          }
        }
      },
    ),

    "chat.params": swallowHookErrors(
      "model capture failed",
      async (input: { sessionID: string; agent?: string; model: { id?: string }; isEnsureTitle?: boolean }) => {
        if (input.isEnsureTitle || input.agent === "title") return
        const model = hookString(input.model?.id).trim()
        if (input.sessionID && model) {
          rememberSessionModel(input.sessionID, model)
        }
      },
    ),

    "tool.execute.before": swallowHookErrors(
      "pre-tool checkpoint failed",
      async (input: ToolHookInput, output?: { args?: unknown }) => {
        const toolName = hookString(input.tool)
        const isTrackedEdit = isEditTool(toolName)
        const isTrackedBash = isBashTool(toolName)
        if (!isTrackedEdit && !isTrackedBash) {
          return
        }

        const callID = hookString(input.callID)
        const sessionID = hookString(input.sessionID)
        if (!callID.trim() || !sessionID.trim()) {
          return
        }
        const toolInput = output?.args ?? input.args
        const toolCwd = resolveCwd(extractToolCwd(asRecord(toolInput)))
        const filePaths = isTrackedEdit ? extractFilePaths(toolInput, toolCwd, toolName) : []
        // An empty edit scope can turn into a repository-wide checkpoint.
        if (isTrackedEdit && filePaths.length === 0) return
        // The checkpoint runs at the repo root, while tool paths may be relative
        // to a subdirectory. Send resolved edit paths, not the original arguments.
        const checkpointInput = isTrackedEdit ? { file_paths: filePaths } : toolInput
        const model = sessionModels.get(sessionID)
        if (model) rememberSessionModel(sessionID, model)
        const key = callKey(sessionID, callID)
        const callInfo = { cwd: toolCwd, sessionID, toolInput: checkpointInput, model }
        // Register before any asynchronous work so cancellation can remove the
        // call while repository discovery or the pre-checkpoint is in flight.
        pendingCalls.delete(key)
        pendingCalls.set(key, callInfo)
        // Some clients omit terminal events for cancelled/failed calls. Bound
        // retained state; an evicted call safely skips its post-checkpoint.
        if (pendingCalls.size > MAX_PENDING_CALLS) {
          const oldestKey = pendingCalls.keys().next().value
          if (oldestKey !== undefined) pendingCalls.delete(oldestKey)
        }

        let checkpointSucceeded = false
        try {
          const repoDir = await resolveRepoDir(filePaths, toolCwd)
          if (!repoDir || pendingCalls.get(key) !== callInfo) return
          callInfo.cwd = isTrackedBash ? toolCwd : repoDir
          await runCheckpoint(JSON.stringify({
            hook_event_name: "PreToolUse",
            session_id: sessionID,
            tool_use_id: callID,
            cwd: callInfo.cwd,
            tool_name: toolName,
            tool_input: checkpointInput,
            model,
          }))
          checkpointSucceeded = true
        } finally {
          if (!checkpointSucceeded && pendingCalls.get(key) === callInfo) {
            pendingCalls.delete(key)
          }
        }
      },
    ),

    "tool.execute.after": swallowHookErrors(
      "post-tool checkpoint failed",
      async (input: ToolHookInput) => {
        const toolName = hookString(input.tool)
        if (!isEditTool(toolName) && !isBashTool(toolName)) {
          return
        }

        const callID = hookString(input.callID)
        const key = callKey(hookString(input.sessionID), callID)
        const callInfo = pendingCalls.get(key)
        pendingCalls.delete(key)

        if (!callInfo) {
          debugLog(`skipping post-tool checkpoint without matching pre-tool call for ${callID}`)
          return
        }

        const hookInput = JSON.stringify({
          hook_event_name: "PostToolUse",
          session_id: callInfo.sessionID,
          tool_use_id: callID,
          cwd: callInfo.cwd,
          tool_name: toolName,
          // Only attribute paths whose pre-edit state was checkpointed. Files
          // discovered in post metadata may already contain unrelated edits.
          tool_input: callInfo.toolInput,
          model: callInfo.model,
        })
        await runCheckpoint(hookInput)
      },
    ),
  }
}

export default GitAiPlugin
