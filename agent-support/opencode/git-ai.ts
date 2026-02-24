/**
 * git-ai plugin for OpenCode
 *
 * This plugin integrates git-ai with OpenCode to track AI-generated code.
 * It uses tool, session, and message lifecycle events to emit telemetry and
 * create checkpoints that mark code changes as human or AI-authored.
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
const MCP_TOOL_PREFIX = "mcp__"

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

  // Track pending edits by callID so we can reference them in the after hook.
  const pendingEdits = new Map<string, { filePath: string; repoDir: string; sessionID: string; toolName: string }>()

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

  const charsCount = (value: unknown): number => {
    if (typeof value !== "string") {
      return 0
    }
    return Array.from(value).length
  }

  const getSessionId = (event: any): string | null => {
    return event?.sessionID ?? event?.sessionId ?? event?.session?.id ?? event?.id ?? null
  }

  const getCwd = (event: any): string => {
    return event?.cwd ?? event?.workspace ?? process.cwd()
  }

  const emitCheckpoint = async (payload: Record<string, unknown>) => {
    try {
      const hookInput = JSON.stringify(payload)
      await $`echo ${hookInput} | ${GIT_AI_BIN} checkpoint opencode --hook-input stdin`.quiet()
    } catch (error) {
      console.error("[git-ai] Failed to emit checkpoint payload:", String(error))
    }
  }

  const emitTelemetryOnly = async (
    hookEventName: string,
    event: any,
    telemetryPayload: Record<string, string> = {},
  ) => {
    const sessionID = getSessionId(event)
    if (!sessionID) {
      return
    }

    await emitCheckpoint({
      hook_event_name: hookEventName,
      hook_source: "opencode_plugin",
      session_id: sessionID,
      cwd: getCwd(event),
      telemetry_payload: telemetryPayload,
    })
  }

  return {
    "session.created": async (event: any) => {
      await emitTelemetryOnly("session.created", event, {
        source: "opencode",
      })
    },

    "session.deleted": async (event: any) => {
      const reason = event?.reason ? String(event.reason) : "completed"
      await emitTelemetryOnly("session.deleted", event, {
        reason,
      })
    },

    "session.idle": async (event: any) => {
      await emitTelemetryOnly("session.idle", event, {
        status: "idle",
      })
    },

    "message.updated": async (event: any) => {
      const role = event?.role ?? event?.message?.role
      const messageText = event?.text ?? event?.message?.text ?? ""
      const messageID = event?.messageID ?? event?.messageId ?? event?.message?.id ?? event?.id
      const telemetryPayload: Record<string, string> = {
        role: typeof role === "string" ? role : "unknown",
      }
      if (typeof messageID === "string" && messageID.length > 0) {
        telemetryPayload.message_id = messageID
      }
      const normalizedRole = typeof role === "string" ? role.toLowerCase() : ""
      const textChars = charsCount(messageText)
      if ((normalizedRole === "user" || normalizedRole === "human") && textChars > 0) {
        telemetryPayload.prompt_char_count = String(textChars)
      } else if (normalizedRole === "assistant" && textChars > 0) {
        telemetryPayload.response_char_count = String(textChars)
      }
      await emitTelemetryOnly("message.updated", event, telemetryPayload)
    },

    "message.part.updated": async (event: any) => {
      const role = event?.role ?? event?.message?.role ?? "assistant"
      const partText = event?.text ?? event?.part?.text ?? ""
      const messageID = event?.messageID ?? event?.messageId ?? event?.message?.id ?? event?.id
      const telemetryPayload: Record<string, string> = {
        role: typeof role === "string" ? role : "assistant",
      }
      if (typeof messageID === "string" && messageID.length > 0) {
        telemetryPayload.message_id = messageID
      }
      const responseChars = charsCount(partText)
      if (responseChars > 0) {
        telemetryPayload.response_char_count = String(responseChars)
      }
      await emitTelemetryOnly("message.part.updated", event, telemetryPayload)
    },

    "tool.execute.before": async (input, output) => {
      const sessionID = input?.sessionID
      if (!sessionID) {
        return
      }

      const toolName = String(input?.tool ?? "unknown")
      const isMcp = toolName.startsWith(MCP_TOOL_PREFIX)
      const filePath = output?.args?.filePath as string | undefined
      const isFileEdit = FILE_EDIT_TOOLS.includes(toolName)

      if (!isFileEdit || !filePath) {
        const telemetryPayload: Record<string, string> = {
          tool_name: toolName,
          tool_use_id: String(input?.callID ?? ""),
        }
        if (isMcp) {
          telemetryPayload.mcp_tool_name = toolName
        }
        await emitTelemetryOnly("tool.execute.before", input, telemetryPayload)
        return
      }

      // Find the git repo for this file
      const repoDir = await findGitRepo(filePath)
      if (!repoDir) {
        await emitTelemetryOnly("tool.execute.before", input, {
          tool_name: toolName,
          tool_use_id: String(input?.callID ?? ""),
        })
        return
      }

      // Store filePath, repoDir, and sessionID for the after hook
      pendingEdits.set(input.callID, { filePath, repoDir, sessionID, toolName })

      await emitCheckpoint({
        hook_event_name: "PreToolUse",
        hook_source: "opencode_plugin",
        session_id: sessionID,
        cwd: repoDir,
        tool_name: toolName,
        tool_input: { filePath },
        telemetry_payload: {
          tool_name: toolName,
          tool_use_id: String(input?.callID ?? ""),
        },
      })
    },

    "tool.execute.after": async (input, output) => {
      // Get the filePath and repoDir we stored in the before hook
      const editInfo = pendingEdits.get(input.callID)
      pendingEdits.delete(input.callID)

      if (!editInfo) {
        const toolName = String(input?.tool ?? "unknown")
        const telemetryPayload: Record<string, string> = {
          tool_name: toolName,
          tool_use_id: String(input?.callID ?? ""),
        }
        if (toolName.startsWith(MCP_TOOL_PREFIX)) {
          telemetryPayload.mcp_tool_name = toolName
        }
        await emitTelemetryOnly("tool.execute.after", input, telemetryPayload)
        return
      }

      const { filePath, repoDir, sessionID, toolName } = editInfo
      const telemetryPayload: Record<string, string> = {
        tool_name: toolName,
        tool_use_id: String(input?.callID ?? ""),
      }
      const durationMs = output?.metadata?.duration_ms ?? output?.metadata?.duration
      if (typeof durationMs === "number") {
        telemetryPayload.duration_ms = String(Math.max(0, Math.floor(durationMs)))
      }

      await emitCheckpoint({
        hook_event_name: "PostToolUse",
        hook_source: "opencode_plugin",
        session_id: sessionID,
        cwd: repoDir,
        tool_name: toolName,
        tool_input: { filePath },
        telemetry_payload: telemetryPayload,
      })
    },
  }
}
