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
import { dirname, isAbsolute, join } from "path"

// Absolute path to git-ai binary, replaced at install time by `git-ai install-hooks`
const GIT_AI_BIN = "__GIT_AI_BINARY_PATH__"

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

export const GitAiPlugin: Plugin = async (ctx) => {
  const { $ } = ctx

  // Check if git-ai is installed
  let gitAiInstalled = false
  try {
    await $`${GIT_AI_BIN} --version`.quiet()
    gitAiInstalled = true
  } catch {
    // git-ai not installed, plugin will be a no-op
  }

  if (!gitAiInstalled) {
    return {}
  }

  // Track pending calls by callID so we can reference them in the after hook
  const pendingCalls = new Map<string, { repoDir: string; sessionID: string; toolInput: unknown }>()

  // Helper to find git repo root from a file path
  const findGitRepo = async (pathHint: string): Promise<string | null> => {
    const candidateDirs = [pathHint, dirname(pathHint)]

    for (const dir of candidateDirs) {
      try {
        const result = await $`git -C ${dir} rev-parse --show-toplevel`.quiet()
        const repoRoot = result.stdout.toString().trim()
        if (repoRoot) {
          return repoRoot
        }
      } catch {
        // try next candidate
      }
    }

    return null
  }

  const resolveRepoDir = async (filePaths: string[], cwd?: string): Promise<string | null> => {
    if (cwd) {
      const fromCwd = await findGitRepo(cwd)
      if (fromCwd) {
        return fromCwd
      }
    }

    const fromProcessCwd = await findGitRepo(process.cwd())
    if (fromProcessCwd) {
      return fromProcessCwd
    }

    for (const filePath of filePaths) {
      const repo = await findGitRepo(filePath)
      if (repo) {
        return repo
      }
    }

    return null
  }

  const extractToolCwd = (inputCwd: unknown, outputArgs: Record<string, unknown> | undefined): string | undefined => {
    if (typeof outputArgs?.workdir === "string") return outputArgs.workdir
    if (typeof outputArgs?.cwd === "string") return outputArgs.cwd
    if (typeof inputCwd === "string") return inputCwd
    return undefined
  }

  return {
    "tool.execute.before": async (input, output) => {
      const toolInput = output.args
      const toolCwd = extractToolCwd((input as { cwd?: unknown }).cwd ?? (input as { workdir?: unknown }).workdir, output.args)

      if (isEditTool(input.tool)) {
        // File-edit tools: extract file paths for rich repo resolution
        const filePaths = extractFilePaths(toolInput, toolCwd)
        const repoDir = await resolveRepoDir(filePaths, toolCwd)
        if (!repoDir) {
          return
        }

        pendingCalls.set(input.callID, { repoDir, sessionID: input.sessionID, toolInput })

        try {
          const hookInput = JSON.stringify({
            hook_event_name: "PreToolUse",
            session_id: input.sessionID,
            tool_use_id: input.callID,
            cwd: repoDir,
            tool_name: input.tool,
            tool_input: toolInput,
          })
          await $`echo ${hookInput} | ${GIT_AI_BIN} checkpoint opencode --hook-input stdin`.quiet()
        } catch (error) {
          console.error("[git-ai] Failed to create human checkpoint:", String(error))
        }

      } else if (isBashTool(input.tool)) {
        // Bash tool: no file paths in input; resolve repo from workdir or cwd
        const repoDir = await resolveRepoDir([], toolCwd)
        if (!repoDir) {
          return
        }

        pendingCalls.set(input.callID, { repoDir, sessionID: input.sessionID, toolInput })

        try {
          const hookInput = JSON.stringify({
            hook_event_name: "PreToolUse",
            session_id: input.sessionID,
            tool_use_id: input.callID,
            cwd: repoDir,
            tool_name: input.tool,
            tool_input: toolInput,
          })
          await $`echo ${hookInput} | ${GIT_AI_BIN} checkpoint opencode --hook-input stdin`.quiet()
        } catch (error) {
          console.error("[git-ai] Failed to create human checkpoint:", String(error))
        }
      }
    },

    "tool.execute.after": async (input, _output) => {
      if (!isEditTool(input.tool) && !isBashTool(input.tool)) {
        return
      }

      const callInfo = pendingCalls.get(input.callID)
      pendingCalls.delete(input.callID)

      if (!callInfo) {
        return
      }

      const { repoDir, sessionID, toolInput } = callInfo

      try {
        const hookInput = JSON.stringify({
          hook_event_name: "PostToolUse",
          session_id: sessionID,
          tool_use_id: input.callID,
          cwd: repoDir,
          tool_name: input.tool,
          tool_input: toolInput,
        })
        await $`echo ${hookInput} | ${GIT_AI_BIN} checkpoint opencode --hook-input stdin`.quiet()
      } catch (error) {
        console.error("[git-ai] Failed to create AI checkpoint:", String(error))
      }
    },
  }
}
