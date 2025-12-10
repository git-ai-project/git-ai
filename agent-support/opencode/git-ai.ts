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
 *   - git-ai must be installed and available in PATH
 *
 * @see https://github.com/acunniffe/git-ai
 * @see https://opencode.ai/docs/plugins/
 */

import type { Plugin } from "@opencode-ai/plugin"
import { dirname } from "path"

// Tools that modify files and should be tracked
const FILE_EDIT_TOOLS = ["edit", "write"]

export const GitAiPlugin: Plugin = async (ctx) => {
  const { $, client } = ctx

  // Check if git-ai is installed
  let gitAiInstalled = false
  try {
    await $`git-ai --version`.quiet()
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

  // Track the active model for each session
  // Updated via chat.params hook
  const sessionModels = new Map<string, string>()

  // Helper to get model info from session
  // Tries the cache first, then falls back to fetching messages
  const getModelFromSession = async (sessionID: string): Promise<string> => {
    // Check cache first (populated by chat.params)
    if (sessionModels.has(sessionID)) {
      return sessionModels.get(sessionID)!
    }

    try {
      // Get recent messages from the session to find model info
      const messages = await client.session.messages({ path: { id: sessionID }, query: { limit: 5 } })
      if (messages.data) {
        // Look for the most recent assistant message which has model info
        for (const msg of messages.data) {
          if (msg.info.role === "assistant") {
            const assistantMsg = msg.info as { providerID?: string; modelID?: string }
            if (assistantMsg.modelID) {
              const providerID = assistantMsg.providerID || "unknown"
              return `${providerID}/${assistantMsg.modelID}`
            }
          }
          // User messages also have model info
          if (msg.info.role === "user") {
            const userMsg = msg.info as { model?: { providerID: string; modelID: string } }
            if (userMsg.model?.modelID) {
              return `${userMsg.model.providerID}/${userMsg.model.modelID}`
            }
          }
        }
      }
    } catch {
      // Ignore errors fetching session messages
    }
    return "unknown"
  }

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
    "chat.params": async (input) => {
      // Update the active model for this session
      const { sessionID, model } = input
      if (model.id) {
        const providerID = model.providerID || "unknown"
        const modelStr = `${providerID}/${model.id}`
        sessionModels.set(sessionID, modelStr)
      }
    },

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
          type: "human",
          repo_working_dir: repoDir,
          will_edit_filepaths: [filePath],
        })

        await $`echo ${hookInput} | git-ai checkpoint agent-v1 --hook-input stdin`.quiet()
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
        // Get model info from session
        const model = await getModelFromSession(sessionID)

        // Create AI checkpoint after edit
        // This marks the changes made by this tool call as AI-authored
        const hookInput = JSON.stringify({
          type: "ai_agent",
          repo_working_dir: repoDir,
          agent_name: "opencode",
          model,
          conversation_id: sessionID,
          edited_filepaths: [filePath],
          transcript: {
            messages: [],
          },
        })

        await $`echo ${hookInput} | git-ai checkpoint agent-v1 --hook-input stdin`.quiet()
      } catch (error) {
        // Log to stderr for debugging, but don't throw - git-ai errors shouldn't break the agent
        console.error("[git-ai] Failed to create AI checkpoint:", String(error))
      }
    },
  }
}
