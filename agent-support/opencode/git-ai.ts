/**
 * git-ai plugin for OpenCode
 *
 * This plugin integrates git-ai with OpenCode to track AI-generated code.
 * It uses the tool.execute.before and tool.execute.after events to create
 * checkpoints that mark code changes as human or AI-authored.
 *
 * Installation:
 *   - Automatically installed by `git-ai install-hooks`
 *   - Or manually copy to ~/.config/opencode/plugin/git-ai.ts (global)
 *   - Or to .opencode/plugin/git-ai.ts (project-local)
 *
 * Requirements:
 *   - git-ai must be installed (path is injected at install time)
 *
 * @see https://github.com/git-ai-project/git-ai
 * @see https://opencode.ai/docs/plugins/
 */

import type { Plugin } from "@opencode-ai/plugin"
import { dirname } from "path"

// Absolute path to git-ai binary, replaced at install time by `git-ai install-hooks`
const GIT_AI_BIN = "__GIT_AI_BINARY_PATH__"

// Bash/shell tool names that need special checkpoint handling
const BASH_TOOLS = ["bash", "shell"]

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

  // Track pending edits by callID so we can reference them in the after hook
  // Stores { filePath, repoDir, sessionID } for each pending edit
  const pendingEdits = new Map<string, { filePath: string; repoDir: string; sessionID: string; bashCommand?: string }>()

  // Helper to find git repo root from a file path
  const findGitRepo = async (filePath: string): Promise<string | null> => {
    try {
      const dir = dirname(filePath)
      const result = await $`git -C ${dir} rev-parse --show-toplevel`.quiet()
      const repoRoot = result.stdout.toString().trim()
      return repoRoot || null
    } catch {
      // Not a git repo or git not available
      return null
    }
  }

  return {
    "tool.execute.before": async (input, output) => {
      // Extract file path from tool arguments (args are in output, not input)
      const filePath = output.args?.filePath as string | undefined

      // For bash/shell tools, extract the command for blacklist evaluation
      const isBashTool = BASH_TOOLS.includes(input.tool)
      const bashCommand = isBashTool
        ? (output.args?.command as string | undefined) ?? (output.args?.input as string | undefined)
        : undefined

      // Determine the working directory
      let repoDir: string | null = null
      if (filePath) {
        repoDir = await findGitRepo(filePath)
      } else if (output.args?.cwd) {
        repoDir = await findGitRepo(output.args.cwd as string)
      } else {
        // Try process cwd as fallback
        try {
          const result = await $`git rev-parse --show-toplevel`.quiet()
          repoDir = result.stdout.toString().trim() || null
        } catch {
          // Not in a git repo
        }
      }

      if (!repoDir) {
        return
      }

      // Store info for the after hook
      pendingEdits.set(input.callID, { filePath: filePath ?? "", repoDir, sessionID: input.sessionID, bashCommand })

      try {
        const hookInput = JSON.stringify({
          hook_event_name: "PreToolUse",
          tool_name: input.tool,
          session_id: input.sessionID,
          cwd: repoDir,
          tool_input: {
            ...(filePath ? { filePath } : {}),
            ...(bashCommand ? { command: bashCommand } : {}),
          },
        })

        await $`echo ${hookInput} | ${GIT_AI_BIN} checkpoint opencode --hook-input stdin`.quiet()
      } catch (error) {
        console.error("[git-ai] Failed to create human checkpoint:", String(error))
      }
    },

    "tool.execute.after": async (input, _output) => {
      // Get the info we stored in the before hook
      const editInfo = pendingEdits.get(input.callID)
      pendingEdits.delete(input.callID)

      if (!editInfo) {
        return
      }

      const { filePath, repoDir, sessionID, bashCommand } = editInfo

      try {
        const hookInput = JSON.stringify({
          hook_event_name: "PostToolUse",
          tool_name: input.tool,
          session_id: sessionID,
          cwd: repoDir,
          tool_input: {
            ...(filePath ? { filePath } : {}),
            ...(bashCommand ? { command: bashCommand } : {}),
          },
        })

        await $`echo ${hookInput} | ${GIT_AI_BIN} checkpoint opencode --hook-input stdin`.quiet()
      } catch (error) {
        console.error("[git-ai] Failed to create AI checkpoint:", String(error))
      }
    },
  }
}
