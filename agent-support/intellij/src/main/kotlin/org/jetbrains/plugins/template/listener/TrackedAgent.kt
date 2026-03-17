package org.jetbrains.plugins.template.listener

data class TrackedAgent(
    val agentName: String,
    val workspaceRoot: String,
    val lastCheckpointContent: String,
    val trackedAt: Long = System.currentTimeMillis(),
    val refreshEligibleUntil: Long = trackedAt + REFRESH_ELIGIBILITY_WINDOW_MS
) {
    companion object {
        const val STALE_THRESHOLD_MS = 300_000L
        const val REFRESH_ELIGIBILITY_WINDOW_MS = 15_000L
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
