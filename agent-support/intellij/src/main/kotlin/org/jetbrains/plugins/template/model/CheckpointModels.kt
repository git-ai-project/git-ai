package org.jetbrains.plugins.template.model

import com.google.gson.GsonBuilder
import com.google.gson.JsonObject

/**
 * Input data for git-ai checkpoint agent-v1 command sent via stdin.
 * Uses a sealed class to represent the two types: human (before edit) and ai_agent (after edit).
 */
sealed class AgentV1Input {
    abstract fun toJson(): String

    /**
     * Human input - used for before_edit events to capture state before AI makes changes.
     */
    data class Human(
        val repoWorkingDir: String,
        val willEditFilepaths: List<String>? = null,
        val dirtyFiles: Map<String, String>? = null
    ) : AgentV1Input() {
        override fun toJson(): String {
            val json = JsonObject()
            json.addProperty("type", "human")
            json.addProperty("repo_working_dir", repoWorkingDir)

            willEditFilepaths?.let { paths ->
                val pathsArray = com.google.gson.JsonArray()
                paths.forEach { pathsArray.add(it) }
                json.add("will_edit_filepaths", pathsArray)
            }

            dirtyFiles?.let { files ->
                val filesObj = JsonObject()
                files.forEach { (path, content) -> filesObj.addProperty(path, content) }
                json.add("dirty_files", filesObj)
            }

            return GsonBuilder().create().toJson(json)
        }
    }

    /**
     * AI Agent input - used for after_edit events to record changes made by an AI agent.
     */
    data class AiAgent(
        val repoWorkingDir: String,
        val editedFilepaths: List<String>? = null,
        val agentName: String,
        val model: String = "unknown",
        val conversationId: String = "",
        val transcript: Transcript = Transcript(),
        val dirtyFiles: Map<String, String>? = null
    ) : AgentV1Input() {
        override fun toJson(): String {
            val json = JsonObject()
            json.addProperty("type", "ai_agent")
            json.addProperty("repo_working_dir", repoWorkingDir)

            editedFilepaths?.let { paths ->
                val pathsArray = com.google.gson.JsonArray()
                paths.forEach { pathsArray.add(it) }
                json.add("edited_filepaths", pathsArray)
            }

            json.addProperty("agent_name", agentName)
            json.addProperty("model", model)
            json.addProperty("conversation_id", conversationId)
            json.add("transcript", transcript.toJsonObject())

            dirtyFiles?.let { files ->
                val filesObj = JsonObject()
                files.forEach { (path, content) -> filesObj.addProperty(path, content) }
                json.add("dirty_files", filesObj)
            }

            return GsonBuilder().create().toJson(json)
        }
    }

    /**
     * Transcript structure for AI agent conversations.
     */
    data class Transcript(
        val messages: List<TranscriptMessage> = emptyList()
    ) {
        fun toJsonObject(): JsonObject {
            val json = JsonObject()
            val messagesArray = com.google.gson.JsonArray()
            messages.forEach { msg -> messagesArray.add(msg.toJsonObject()) }
            json.add("messages", messagesArray)
            return json
        }
    }

    /**
     * A single message in a transcript.
     */
    data class TranscriptMessage(
        val role: String,
        val content: String
    ) {
        fun toJsonObject(): JsonObject {
            val json = JsonObject()
            json.addProperty("role", role)
            json.addProperty("content", content)
            return json
        }
    }
}
