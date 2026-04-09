package org.jetbrains.plugins.template.listener

import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.diagnostic.Logger
import com.intellij.openapi.vfs.LocalFileSystem
import com.intellij.openapi.vfs.newvfs.BulkFileListener
import com.intellij.openapi.vfs.newvfs.events.VFileContentChangeEvent
import com.intellij.openapi.vfs.newvfs.events.VFileEvent
import org.jetbrains.plugins.template.model.KnownHumanInput
import org.jetbrains.plugins.template.services.GitAiService
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.ScheduledExecutorService
import java.util.concurrent.ScheduledFuture
import java.util.concurrent.TimeUnit

/**
 * Listens for IDE-initiated document saves (isFromRefresh=false) and fires a
 * known_human checkpoint after a 500ms debounce window.
 *
 * Filters out JetBrains-internal paths (e.g. .idea/, .sandbox/) to avoid noise.
 */
class DocumentSaveListener(
    private val scheduler: ScheduledExecutorService,
    private val editorVersion: String,
    private val extensionVersion: String,
) : BulkFileListener {

    private val logger = Logger.getInstance(DocumentSaveListener::class.java)

    private val debounceMs = 500L

    // Per workspace root: debounce future
    private val pendingFutures = ConcurrentHashMap<String, ScheduledFuture<*>>()

    // Per workspace root: set of absolute paths pending in the current debounce window
    private val pendingPaths = ConcurrentHashMap<String, MutableSet<String>>()

    override fun after(events: List<VFileEvent>) {
        val workspaceRoots = mutableSetOf<String>()

        for (event in events) {
            if (event !is VFileContentChangeEvent) continue
            if (event.isFromRefresh) continue  // external writes handled by VfsRefreshListener
            if (isInternalJetBrainsPath(event.path)) {
                logger.debug("[SAVE] Ignoring internal JetBrains file: ${event.path}")
                continue
            }

            val workspaceRoot = findWorkspaceRoot(event.path) ?: continue

            pendingPaths
                .computeIfAbsent(workspaceRoot) { ConcurrentHashMap.newKeySet() }
                .add(event.path)
            workspaceRoots.add(workspaceRoot)

            logger.warn("[SAVE] Document saved: ${event.path}")
        }

        for (root in workspaceRoots) {
            scheduleCheckpoint(root)
        }
    }

    private fun scheduleCheckpoint(workspaceRoot: String) {
        pendingFutures[workspaceRoot]?.cancel(false)
        val future = scheduler.schedule({
            executeCheckpoint(workspaceRoot)
        }, debounceMs, TimeUnit.MILLISECONDS)
        pendingFutures[workspaceRoot] = future
    }

    private fun executeCheckpoint(workspaceRoot: String) {
        pendingFutures.remove(workspaceRoot)

        val paths = pendingPaths[workspaceRoot] ?: return
        val snapshot = paths.toList()
        snapshot.forEach { paths.remove(it) }
        if (snapshot.isEmpty()) return

        val dirtyFiles = mutableMapOf<String, String>()
        ApplicationManager.getApplication().runReadAction {
            for (absolutePath in snapshot) {
                val content = LocalFileSystem.getInstance().findFileByPath(absolutePath)
                    ?.let { String(it.contentsToByteArray(), Charsets.UTF_8) }
                if (content != null) {
                    dirtyFiles[absolutePath] = content
                }
            }
        }

        if (dirtyFiles.isEmpty()) return

        val input = KnownHumanInput(
            editor = "jetbrains",
            editorVersion = editorVersion,
            extensionVersion = extensionVersion,
            cwd = workspaceRoot,
            editedFilepaths = dirtyFiles.keys.toList(),
            dirtyFiles = dirtyFiles
        )

        logger.warn("[SAVE] Firing known_human checkpoint for ${dirtyFiles.keys}")
        GitAiService.getInstance().checkpointKnownHuman(input, workspaceRoot)
    }

    private fun isInternalJetBrainsPath(path: String): Boolean {
        return path.contains("/.idea/") ||
               path.contains("/.sandbox/") ||
               path.contains("/system/projects/")
    }

    private fun findWorkspaceRoot(absolutePath: String): String? {
        var current = LocalFileSystem.getInstance().findFileByPath(absolutePath)?.parent
        while (current != null) {
            if (current.findChild(".git") != null) {
                return current.path
            }
            current = current.parent
        }
        return null
    }
}
