package org.jetbrains.plugins.template.services

import com.intellij.openapi.Disposable
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.components.Service
import com.intellij.openapi.diagnostic.thisLogger
import com.intellij.openapi.editor.EditorFactory
import com.intellij.openapi.vfs.VirtualFileManager
import org.jetbrains.plugins.template.listener.DocumentChangeListener
import org.jetbrains.plugins.template.listener.TrackedAgent
import org.jetbrains.plugins.template.listener.VfsRefreshListener
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit

/**
 * Application-level service that registers the DocumentChangeListener and VfsRefreshListener
 * to track document changes and identify AI agent plugins that triggered them.
 *
 * Owns the shared state (agent-tracked files map and scheduler) injected into both listeners
 * to separate AI edit detection (stack trace) from disk change detection (VFS refresh).
 */
@Service(Service.Level.APP)
class DocumentChangeTrackerService : Disposable {

    // Shared scheduler for debouncing checkpoints across both listeners
    private val scheduler = Executors.newSingleThreadScheduledExecutor { r ->
        Thread(r, "git-ai-checkpoint-scheduler").apply { isDaemon = true }
    }

    // Shared tracking state — files recently touched by AI agents, keyed by absolute path
    private val agentTouchedFiles = ConcurrentHashMap<String, TrackedAgent>()

    init {
        thisLogger().warn("DocumentChangeTrackerService initializing...")

        val docListener = DocumentChangeListener(agentTouchedFiles, scheduler)
        EditorFactory.getInstance().eventMulticaster.addDocumentListener(docListener, this)

        val vfsListener = VfsRefreshListener(agentTouchedFiles, scheduler)
        ApplicationManager.getApplication().messageBus.connect(this)
            .subscribe(VirtualFileManager.VFS_CHANGES, vfsListener)

        // Periodic eviction of stale tracking entries
        scheduler.scheduleAtFixedRate(
            { evictStaleEntries() },
            TrackedAgent.STALE_THRESHOLD_MS, TrackedAgent.STALE_THRESHOLD_MS, TimeUnit.MILLISECONDS
        )

        thisLogger().warn("DocumentChangeTrackerService initialized - tracking document changes and VFS refreshes")
    }

    private fun evictStaleEntries() {
        val now = System.currentTimeMillis()
        agentTouchedFiles.entries.removeIf { entry ->
            now - entry.value.trackedAt > TrackedAgent.STALE_THRESHOLD_MS
        }
    }

    override fun dispose() {
        scheduler.shutdownNow()
        thisLogger().warn("DocumentChangeTrackerService disposed")
    }
}
