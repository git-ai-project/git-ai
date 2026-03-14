package org.jetbrains.plugins.template.listener

data class TrackedAgent(
    val agentName: String,
    val workspaceRoot: String,
    val lastCheckpointContent: String,
    val trackedAt: Long = System.currentTimeMillis()
) {
    companion object {
        const val STALE_THRESHOLD_MS = 300_000L
    }
}

/**
 * Converts an absolute file path to a path relative to the workspace root.
 */
fun toRelativePath(absolutePath: String, workspaceRoot: String): String {
    return if (absolutePath.startsWith(workspaceRoot)) {
        absolutePath.removePrefix(workspaceRoot).removePrefix("/")
    } else {
        absolutePath
    }
}
