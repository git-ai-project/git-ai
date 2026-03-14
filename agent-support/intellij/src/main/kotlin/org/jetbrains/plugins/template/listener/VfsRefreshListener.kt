package org.jetbrains.plugins.template.listener

import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.diagnostic.Logger
import com.intellij.openapi.vfs.LocalFileSystem
import com.intellij.openapi.vfs.newvfs.BulkFileListener
import com.intellij.openapi.vfs.newvfs.events.VFileContentChangeEvent
import com.intellij.openapi.vfs.newvfs.events.VFileEvent
import org.jetbrains.plugins.template.model.AgentV1Input
import org.jetbrains.plugins.template.services.GitAiService
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.ScheduledExecutorService
import java.util.concurrent.ScheduledFuture
import java.util.concurrent.TimeUnit

/**
 * Listens for VFS refresh events to detect AI agent disk writes on tracked files.
 * Only fires on actual disk changes (isFromRefresh == true), never on in-editor edits.
 *
 * This eliminates false positives from human typing, IDE refactoring, and VCS operations
 * that the DocumentChangeListener's document-level events cannot distinguish.
 */
class VfsRefreshListener(
    private val agentTouchedFiles: ConcurrentHashMap<String, TrackedAgent>,
    private val scheduler: ScheduledExecutorService,
) : BulkFileListener {

    private val logger = Logger.getInstance(VfsRefreshListener::class.java)

    // Sweep checkpoint debounce (5 seconds) - batches VFS refresh events
    private val sweepDebounceMs = 5000L

    // Pending sweep checkpoints per workspace root (debounced)
    private val pendingSweeps = ConcurrentHashMap<String, ScheduledFuture<*>>()

    override fun after(events: List<VFileEvent>) {
        val workspaceRootsToSweep = mutableSetOf<String>()

        for (event in events) {
            if (event !is VFileContentChangeEvent) continue
            if (!event.isFromRefresh) continue
            val tracked = agentTouchedFiles[event.path] ?: continue
            workspaceRootsToSweep.add(tracked.workspaceRoot)
        }

        for (root in workspaceRootsToSweep) {
            scheduleSweepCheckpoint(root)
        }
    }

    private fun scheduleSweepCheckpoint(workspaceRoot: String) {
        pendingSweeps[workspaceRoot]?.cancel(false)
        val future = scheduler.schedule({
            executeSweepCheckpoint(workspaceRoot)
        }, sweepDebounceMs, TimeUnit.MILLISECONDS)
        pendingSweeps[workspaceRoot] = future
    }

    private fun executeSweepCheckpoint(workspaceRoot: String) {
        pendingSweeps.remove(workspaceRoot)

        val filesToSweep = agentTouchedFiles.entries
            .filter { it.value.workspaceRoot == workspaceRoot }
            .toList()

        if (filesToSweep.isEmpty()) return

        val now = System.currentTimeMillis()

        data class SweepEntry(val relativePath: String, val content: String)
        val entriesByAgent = mutableMapOf<String, MutableList<SweepEntry>>()

        for ((absolutePath, tracked) in filesToSweep) {
            if (now - tracked.trackedAt > TrackedAgent.STALE_THRESHOLD_MS) {
                agentTouchedFiles.remove(absolutePath)
                continue
            }

            val relativePath = toRelativePath(absolutePath, workspaceRoot)
            val content = ApplicationManager.getApplication().runReadAction<String?> {
                LocalFileSystem.getInstance().findFileByPath(absolutePath)
                    ?.let { String(it.contentsToByteArray(), Charsets.UTF_8) }
            } ?: continue

            if (content == tracked.lastCheckpointContent) continue

            agentTouchedFiles.remove(absolutePath)
            entriesByAgent.getOrPut(tracked.agentName) { mutableListOf() }
                .add(SweepEntry(relativePath, content))
        }

        val service = GitAiService.getInstance()
        for ((agent, entries) in entriesByAgent) {
            val input = AgentV1Input.AiAgent(
                repoWorkingDir = workspaceRoot,
                editedFilepaths = entries.map { it.relativePath },
                agentName = agent,
                conversationId = service.sessionId,
                dirtyFiles = entries.associate { it.relativePath to it.content }
            )

            logger.warn("Triggering sweep checkpoint for $agent on ${entries.size} file(s): ${entries.map { it.relativePath }}")

            service.checkpoint(input, workspaceRoot)
        }
    }
}
