package org.jetbrains.plugins.template.blame

import com.google.gson.annotations.SerializedName

/**
 * JSON output structure from `git-ai blame --json`.
 */
data class BlameJsonOutput(
    val lines: Map<String, String> = emptyMap(),
    val prompts: Map<String, PromptRecord> = emptyMap(),
    val metadata: BlameMetadata? = null
)

data class BlameMetadata(
    @SerializedName("is_logged_in") val isLoggedIn: Boolean = false,
    @SerializedName("current_user") val currentUser: String? = null
)

data class PromptRecord(
    @SerializedName("agent_id") val agentId: AgentId? = null,
    @SerializedName("human_author") val humanAuthor: String? = null,
    val messages: List<PromptMessage>? = null,
    @SerializedName("total_additions") val totalAdditions: Int? = null,
    @SerializedName("total_deletions") val totalDeletions: Int? = null,
    @SerializedName("accepted_lines") val acceptedLines: Int? = null,
    @SerializedName("other_files") val otherFiles: List<String>? = null,
    val commits: List<String>? = null,
    @SerializedName("messages_url") val messagesUrl: String? = null
)

data class AgentId(
    val tool: String = "",
    val id: String = "",
    val model: String = ""
)

data class PromptMessage(
    val type: String = "",
    val text: String? = null,
    val timestamp: String? = null
)

/**
 * Per-line blame information after expanding line ranges.
 */
data class LineBlameInfo(
    val promptHash: String,
    val promptRecord: PromptRecord
)

/**
 * Complete blame result for a file.
 */
data class BlameResult(
    val lineAuthors: Map<Int, LineBlameInfo>,
    val prompts: Map<String, PromptRecord>,
    val metadata: BlameMetadata?,
    val timestamp: Long,
    val totalLines: Int
)

enum class BlameMode {
    OFF, LINE, ALL
}
