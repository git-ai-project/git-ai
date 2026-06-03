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

import type { Plugin } from "@opencode-ai/plugin"
import { spawn } from "child_process"
import { existsSync, readFileSync, statSync } from "fs"
import { dirname, isAbsolute, join, resolve } from "path"

// Absolute path to git-ai binary, replaced at install time by `git-ai install-hooks`
const GIT_AI_BIN = "__GIT_AI_BINARY_PATH__"
const CHECKPOINT_TIMEOUT_MS = 30_000

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

type ToolHookInput = {
  tool?: unknown
  sessionID?: unknown
  callID?: unknown
  args?: unknown
  metadata?: unknown
  cwd?: unknown
  workdir?: unknown
}

const asRecord = (value: unknown): Record<string, unknown> | undefined => {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    return undefined
  }

  return value as Record<string, unknown>
}

const hookString = (value: unknown): string => typeof value === "string" ? value : ""

const extractToolCwd = (inputCwd: unknown, args: Record<string, unknown> | undefined): string | undefined => {
  if (typeof args?.workdir === "string") return args.workdir
  if (typeof args?.cwd === "string") return args.cwd
  if (typeof inputCwd === "string") return inputCwd
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

  const detail = error instanceof Error
    ? `${error.name}: ${error.message}`
    : error === undefined
      ? ""
      : String(error)
  console.error(`[git-ai opencode] ${message}${detail ? `: ${detail}` : ""}`)
}

type CommandOptions = {
  cwd?: string
  input?: string
  timeoutMs?: number
}

const runCommand = (
  command: string,
  args: string[],
  options: CommandOptions = {},
): Promise<{ stdout: string; stderr: string }> => {
  return new Promise((resolve, reject) => {
    let settled = false
    let timeout: ReturnType<typeof setTimeout> | undefined
    const finish = (error: Error | null, output?: { stdout: string; stderr: string }): void => {
      if (settled) {
        return
      }

      settled = true
      if (timeout) {
        clearTimeout(timeout)
      }
      if (error) {
        reject(error)
      } else if (output) {
        resolve(output)
      }
    }

    const child = spawn(command, args, {
      cwd: options.cwd,
      stdio: ["pipe", "pipe", "pipe"],
    })

    const timeoutMs = options.timeoutMs ?? CHECKPOINT_TIMEOUT_MS
    if (timeoutMs > 0) {
      timeout = setTimeout(() => {
        try {
          child.kill("SIGTERM")
        } catch (error) {
          debugLog(`failed to kill timed-out command ${command}`, error)
        }
        finish(new Error(`${command} ${args.join(" ")} timed out after ${timeoutMs}ms`))
      }, timeoutMs)
    }

    const stdout: Buffer[] = []
    const stderr: Buffer[] = []

    child.stdout.on("data", (chunk: Buffer) => stdout.push(chunk))
    child.stderr.on("data", (chunk: Buffer) => stderr.push(chunk))
    child.stdin.on("error", () => {
      // The child may exit before stdin is fully written; close/error handling below reports failures.
    })
    child.on("error", finish)
    child.on("close", (code) => {
      const output = {
        stdout: Buffer.concat(stdout).toString(),
        stderr: Buffer.concat(stderr).toString(),
      }
      if (code === 0) {
        finish(null, output)
        return
      }

      finish(new Error(`${command} ${args.join(" ")} exited with ${code}: ${output.stderr}`))
    })

    if (options.input !== undefined) {
      child.stdin.end(options.input)
    } else {
      child.stdin.end()
    }
  })
}

export const GitAiPlugin: Plugin = async (ctx) => {
  const { worktree, directory } = ctx
  const defaultCwd = worktree || directory || process.cwd()

  // Track pending calls by callID so we can reference them in the after hook
  const pendingCalls = new Map<string, { repoDir: string; sessionID: string; toolInput: unknown }>()

  const nearestExistingDirectory = (pathHint: string): string | null => {
    let candidate = pathHint
    while (candidate) {
      try {
        if (existsSync(candidate)) {
          const stat = statSync(candidate)
          return stat.isDirectory() ? candidate : dirname(candidate)
        }
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

  const isGitDirPointer = (gitFilePath: string, worktreeDir: string): boolean => {
    try {
      const firstLine = readFileSync(gitFilePath, "utf8").split(/\r?\n/, 1)[0]?.trim() ?? ""
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
      return existsSync(gitDirPath)
    } catch (error) {
      debugLog(`failed to read gitdir pointer from ${gitFilePath}`, error)
      return false
    }
  }

  const hasGitMetadata = (dir: string): boolean => {
    const marker = join(dir, ".git")
    try {
      if (!existsSync(marker)) {
        return false
      }

      const stat = statSync(marker)
      if (stat.isDirectory()) {
        return true
      }

      if (stat.isFile()) {
        return isGitDirPointer(marker, dir)
      }
    } catch (error) {
      debugLog(`failed to inspect git metadata at ${marker}`, error)
    }

    return false
  }

  // Helper to find git repo root from a file path or directory
  const findGitRepo = (pathHint: string): string | null => {
    let dir = nearestExistingDirectory(pathHint)
    while (dir) {
      if (hasGitMetadata(dir)) {
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

  const resolveRepoDir = (filePaths: string[], cwd?: string): string | null => {
    const seenHints = new Set<string>()
    const findGitRepoOnce = (pathHint: string | undefined): string | null => {
      if (!pathHint || seenHints.has(pathHint)) {
        return null
      }

      seenHints.add(pathHint)
      return findGitRepo(pathHint)
    }

    for (const filePath of filePaths) {
      const repo = findGitRepoOnce(filePath)
      if (repo) {
        return repo
      }
    }

    const fromCwd = findGitRepoOnce(cwd)
    if (fromCwd) {
      return fromCwd
    }

    const fromDefaultCwd = findGitRepoOnce(defaultCwd)
    if (fromDefaultCwd) {
      return fromDefaultCwd
    }

    const fromProcessCwd = findGitRepoOnce(process.cwd())
    if (fromProcessCwd) {
      return fromProcessCwd
    }

    return null
  }

  const extractMetadataFilePaths = (metadata: unknown, cwd?: string): string[] => {
    if (!metadata || typeof metadata !== "object") {
      return []
    }

    const files = (metadata as { files?: unknown }).files
    if (!Array.isArray(files)) {
      return []
    }

    const paths = new Set<string>()
    for (const file of files) {
      if (!file || typeof file !== "object") {
        continue
      }

      const filePath = (file as { filePath?: unknown; path?: unknown }).filePath ?? (file as { path?: unknown }).path
      if (typeof filePath === "string") {
        const normalized = normalizePath(filePath, cwd ?? defaultCwd)
        if (normalized) {
          paths.add(normalized)
        }
      }
    }

    return [...paths]
  }

  const withMetadataFilePaths = (toolInput: unknown, filePaths: string[]): unknown => {
    if (filePaths.length === 0) {
      return toolInput
    }

    if (toolInput && typeof toolInput === "object" && !Array.isArray(toolInput)) {
      return {
        ...toolInput,
        file_paths: filePaths,
      }
    }

    return {
      input: toolInput,
      file_paths: filePaths,
    }
  }

  return {
    "tool.execute.before": async (input: ToolHookInput, output?: { args?: unknown }) => {
      try {
        const toolName = hookString(input.tool)
        const callID = hookString(input.callID)
        const sessionID = hookString(input.sessionID)
        const toolInput = output?.args ?? input.args
        const toolCwd = resolveCwd(extractToolCwd(input.cwd ?? input.workdir, asRecord(toolInput)))

        if (isEditTool(toolName)) {
          const filePaths = extractFilePaths(toolInput, toolCwd)
          const repoDir = resolveRepoDir(filePaths, toolCwd)
          if (!repoDir) {
            return
          }

          pendingCalls.set(callID, { repoDir, sessionID, toolInput })

          const hookInput = JSON.stringify({
            hook_event_name: "PreToolUse",
            session_id: sessionID,
            tool_use_id: callID,
            cwd: repoDir,
            tool_name: toolName,
            tool_input: toolInput,
          })
          await runCommand(GIT_AI_BIN, ["checkpoint", "opencode", "--hook-input", "stdin"], {
            input: hookInput,
            timeoutMs: CHECKPOINT_TIMEOUT_MS,
          })

        } else if (isBashTool(toolName)) {
          const repoDir = resolveRepoDir([], toolCwd)
          if (!repoDir) {
            return
          }

          pendingCalls.set(callID, { repoDir, sessionID, toolInput })

          const hookInput = JSON.stringify({
            hook_event_name: "PreToolUse",
            session_id: sessionID,
            tool_use_id: callID,
            cwd: repoDir,
            tool_name: toolName,
            tool_input: toolInput,
          })
          await runCommand(GIT_AI_BIN, ["checkpoint", "opencode", "--hook-input", "stdin"], {
            input: hookInput,
            timeoutMs: CHECKPOINT_TIMEOUT_MS,
          })
        }
      } catch (error) {
        debugLog("pre-tool checkpoint failed", error)
        // Checkpoint failures are non-critical — never propagate to the host
      }
    },

    "tool.execute.after": async (input: ToolHookInput, output?: { metadata?: unknown }) => {
      try {
        const toolName = hookString(input.tool)
        if (!isEditTool(toolName) && !isBashTool(toolName)) {
          return
        }

        const callID = hookString(input.callID)
        const callInfo = pendingCalls.get(callID)
        pendingCalls.delete(callID)

        if (!callInfo) {
          debugLog(`skipping post-tool checkpoint without matching pre-tool call for ${callID}`)
          return
        }

        const toolCwd = resolveCwd(extractToolCwd(input.cwd ?? input.workdir, asRecord(input.args)))
        const metadataFilePaths = extractMetadataFilePaths(output?.metadata ?? input.metadata, toolCwd)
        const toolInput = withMetadataFilePaths(callInfo.toolInput, metadataFilePaths)

        const hookInput = JSON.stringify({
          hook_event_name: "PostToolUse",
          session_id: callInfo.sessionID,
          tool_use_id: callID,
          cwd: callInfo.repoDir,
          tool_name: toolName,
          tool_input: toolInput,
        })
        await runCommand(GIT_AI_BIN, ["checkpoint", "opencode", "--hook-input", "stdin"], {
          input: hookInput,
          timeoutMs: CHECKPOINT_TIMEOUT_MS,
        })
      } catch (error) {
        debugLog("post-tool checkpoint failed", error)
        // Checkpoint failures are non-critical — never propagate to the host
      }
    },
  }
}

export default GitAiPlugin
