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

// Tools that modify files and should be tracked
const FILE_EDIT_TOOLS = ["edit", "write"]

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
  const pendingEdits = new Map<string, { filePath: string; repoDir: string; sessionID: string }>()

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
      // Only intercept file editing tools
      if (!FILE_EDIT_TOOLS.includes(input.tool)) {
        return
      }

      // Extract file path from tool arguments (args are in output, not input)
      const filePath = output.args?.filePath as string | undefined
      if (!filePath) {
        return
      }

      // Find the git repo for this file
      const repoDir = await findGitRepo(filePath)
      if (!repoDir) {
        // File is not in a git repo, skip silently
        return
      }

      // Store filePath, repoDir, and sessionID for the after hook
      pendingEdits.set(input.callID, { filePath, repoDir, sessionID: input.sessionID })

      try {
        // Create human checkpoint before AI edit
        // This marks any changes since the last checkpoint as human-authored
        const hookInput = JSON.stringify({
          hook_event_name: "PreToolUse",
          session_id: input.sessionID,
          cwd: repoDir,
          tool_input: { filePath },
        })

        await $`echo ${hookInput} | ${GIT_AI_BIN} checkpoint opencode --hook-input stdin`.quiet()
      } catch (error) {
        // Log to stderr for debugging, but don't throw - git-ai errors shouldn't break the agent
        console.error("[git-ai] Failed to create human checkpoint:", String(error))
      }
    },

    "tool.execute.after": async (input, _output) => {
      // Only intercept file editing tools
      if (!FILE_EDIT_TOOLS.includes(input.tool)) {
        return
      }

      // Get the filePath and repoDir we stored in the before hook
      const editInfo = pendingEdits.get(input.callID)
      pendingEdits.delete(input.callID)

      if (!editInfo) {
        return
      }

      const { filePath, repoDir, sessionID } = editInfo

      try {
        // Create AI checkpoint after edit
        // This marks the changes made by this tool call as AI-authored
        // Transcript is fetched from OpenCode's local storage by the preset
        const hookInput = JSON.stringify({
          hook_event_name: "PostToolUse",
          session_id: sessionID,
          cwd: repoDir,
          tool_input: { filePath },
        })

        await $`echo ${hookInput} | ${GIT_AI_BIN} checkpoint opencode --hook-input stdin`.quiet()
      } catch (error) {
        // Log to stderr for debugging, but don't throw - git-ai errors shouldn't break the agent
        console.error("[git-ai] Failed to create AI checkpoint:", String(error))
      }
    },
  }
}
