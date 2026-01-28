package org.jetbrains.plugins.template.listener

import com.intellij.openapi.diagnostic.Logger
import com.intellij.openapi.editor.event.BulkAwareDocumentListener
import com.intellij.openapi.editor.event.DocumentEvent
import com.intellij.openapi.fileEditor.FileDocumentManager
import com.intellij.openapi.project.ProjectManager
import com.intellij.openapi.vfs.VirtualFile
import org.jetbrains.plugins.template.model.AgentV1Input
import org.jetbrains.plugins.template.services.GitAiService
import java.time.LocalDateTime
import java.time.format.DateTimeFormatter
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.Executors
import java.util.concurrent.ScheduledFuture
import java.util.concurrent.TimeUnit

/**
 * Listens for document changes and triggers git-ai checkpoints when AI agents
 * (like GitHub Copilot) make edits.
 */
class DocumentChangeListener : BulkAwareDocumentListener.Simple {

    private val logger = Logger.getInstance(DocumentChangeListener::class.java)
    private val dateFormatter = DateTimeFormatter.ISO_LOCAL_DATE_TIME

    // Debounce scheduler for batching rapid changes
    private val scheduler = Executors.newSingleThreadScheduledExecutor { r ->
        Thread(r, "git-ai-checkpoint-scheduler").apply { isDaemon = true }
    }

    // Track pending after_edit checkpoints per file (for debouncing)
    private val pendingCheckpoints = ConcurrentHashMap<String, PendingCheckpoint>()

    // Track file content before AI edits start (for dirty_files)
    private val fileContentBeforeEdit = ConcurrentHashMap<String, String>()

    // Track files with pending before_edit checkpoint already triggered
    private val beforeEditTriggered = ConcurrentHashMap<String, Long>()

    // Debounce window in milliseconds
    private val debounceMs = 300L

    // Before edit trigger expiry (5 seconds) - after this, allow new before_edit
    private val beforeEditExpiryMs = 5000L

    data class PendingCheckpoint(
        val filePath: String,
        val workspaceRoot: String,
        val agentName: String,
        val scheduledFuture: ScheduledFuture<*>,
        val contentAfterEdit: String
    )

    override fun beforeDocumentChange(event: DocumentEvent) {
        val stackTrace = Thread.currentThread().stackTrace
        val analysis = StackTraceAnalyzer.analyze(stackTrace)

        val document = event.document
        val file = FileDocumentManager.getInstance().getFile(document) ?: return
        val filePath = file.path

        // Log for debugging
        logDocumentChange("BEFORE", event, analysis)

        // Only proceed for HIGH confidence AI agent detection
        if (analysis.confidence != StackTraceAnalyzer.Confidence.HIGH || analysis.sourceName == null) {
            return
        }

        val workspaceRoot = findWorkspaceRoot(file) ?: return

        // Check if we already triggered before_edit recently for this file
        val lastTriggered = beforeEditTriggered[filePath]
        val now = System.currentTimeMillis()
        if (lastTriggered != null && (now - lastTriggered) < beforeEditExpiryMs) {
            // Already have a pending before_edit, skip
            return
        }

        // Save current file content before the edit
        val currentContent = document.text
        fileContentBeforeEdit[filePath] = currentContent
        beforeEditTriggered[filePath] = now

        // Trigger before_edit checkpoint
        triggerBeforeEditCheckpoint(
            agentName = analysis.sourceName,
            filePath = filePath,
            workspaceRoot = workspaceRoot,
            fileContent = currentContent
        )
    }

    override fun documentChanged(event: DocumentEvent) {
        val stackTrace = Thread.currentThread().stackTrace
        val analysis = StackTraceAnalyzer.analyze(stackTrace)

        val document = event.document
        val file = FileDocumentManager.getInstance().getFile(document) ?: return
        val filePath = file.path

        // Log for debugging
        logDocumentChange("AFTER", event, analysis)

        // Only proceed for HIGH confidence AI agent detection
        if (analysis.confidence != StackTraceAnalyzer.Confidence.HIGH || analysis.sourceName == null) {
            return
        }

        val workspaceRoot = findWorkspaceRoot(file) ?: return
        val contentAfterEdit = document.text

        // Cancel any existing pending checkpoint for this file
        pendingCheckpoints[filePath]?.scheduledFuture?.cancel(false)

        // Schedule debounced after_edit checkpoint
        val future = scheduler.schedule({
            executeAfterEditCheckpoint(filePath)
        }, debounceMs, TimeUnit.MILLISECONDS)

        pendingCheckpoints[filePath] = PendingCheckpoint(
            filePath = filePath,
            workspaceRoot = workspaceRoot,
            agentName = analysis.sourceName,
            scheduledFuture = future,
            contentAfterEdit = contentAfterEdit
        )
    }

    private fun triggerBeforeEditCheckpoint(
        agentName: String,
        filePath: String,
        workspaceRoot: String,
        fileContent: String
    ) {
        // Convert absolute path to relative path for git-ai
        val relativePath = toRelativePath(filePath, workspaceRoot)

        val input = AgentV1Input.Human(
            repoWorkingDir = workspaceRoot,
            willEditFilepaths = listOf(relativePath),
            dirtyFiles = mapOf(relativePath to fileContent)
        )

        logger.warn("Triggering human checkpoint (before edit by $agentName) on $relativePath")

        // Run checkpoint in background to avoid blocking EDT
        scheduler.execute {
            GitAiService.getInstance().checkpoint(input, workspaceRoot)
        }
    }

    private fun executeAfterEditCheckpoint(filePath: String) {
        val pending = pendingCheckpoints.remove(filePath) ?: return
        val contentBefore = fileContentBeforeEdit.remove(filePath)
        beforeEditTriggered.remove(filePath)

        // Convert absolute path to relative path for git-ai
        val relativePath = toRelativePath(pending.filePath, pending.workspaceRoot)

        // Build dirty_files map with current content using relative path
        val dirtyFiles = mutableMapOf(relativePath to pending.contentAfterEdit)
        if (contentBefore != null && contentBefore != pending.contentAfterEdit) {
            // Content actually changed
        }

        val input = AgentV1Input.AiAgent(
            repoWorkingDir = pending.workspaceRoot,
            editedFilepaths = listOf(relativePath),
            agentName = pending.agentName,  // Already formatted by StackTraceAnalyzer
            conversationId = GitAiService.getInstance().sessionId,
            dirtyFiles = dirtyFiles
        )

        logger.warn("Triggering ai_agent checkpoint for ${pending.agentName} on $relativePath")

        GitAiService.getInstance().checkpoint(input, pending.workspaceRoot)
    }

    /**
     * Converts an absolute file path to a path relative to the workspace root.
     */
    private fun toRelativePath(absolutePath: String, workspaceRoot: String): String {
        return if (absolutePath.startsWith(workspaceRoot)) {
            absolutePath.removePrefix(workspaceRoot).removePrefix("/")
        } else {
            absolutePath
        }
    }

    private fun findWorkspaceRoot(file: VirtualFile): String? {
        // Try to find an open project containing this file
        for (project in ProjectManager.getInstance().openProjects) {
            val basePath = project.basePath
            if (basePath != null && file.path.startsWith(basePath)) {
                return basePath
            }
        }

        // Fallback: walk up to find .git directory
        var current = file.parent
        while (current != null) {
            if (current.findChild(".git") != null) {
                return current.path
            }
            current = current.parent
        }

        logger.warn("Could not find workspace root for ${file.path}")
        return null
    }

    private fun logDocumentChange(phase: String, event: DocumentEvent, analysis: StackTraceAnalyzer.AnalysisResult) {
        val document = event.document
        val file = FileDocumentManager.getInstance().getFile(document)
        val filePath = file?.path ?: "<unknown>"

        val oldText = truncateText(event.oldFragment.toString())
        val newText = truncateText(event.newFragment.toString())

        val logMessage = buildString {
            appendLine()
            appendLine("================================================================================")
            appendLine("DOCUMENT CHANGE [$phase] at ${LocalDateTime.now().format(dateFormatter)}")
            appendLine("--------------------------------------------------------------------------------")
            appendLine("File: $filePath")
            appendLine("Offset: ${event.offset} | Old Length: ${event.oldLength} | New Length: ${event.newLength}")
            appendLine()
            appendLine("Old Text: $oldText")
            appendLine("New Text: $newText")
            appendLine()

            if (analysis.sourceName != null) {
                appendLine("DETECTED SOURCE: ${analysis.sourceName}")
                appendLine("CONFIDENCE: ${analysis.confidence}")
                appendLine()
                appendLine("STACK TRACE (relevant frames):")
                appendLine(StackTraceAnalyzer.formatRelevantFrames(analysis.relevantFrames))
                appendLine()
            } else {
                appendLine("DETECTED SOURCE: User/IDE action (no AI agent detected)")
                appendLine("CONFIDENCE: ${analysis.confidence}")
                appendLine()
            }

            appendLine("================================================================================")
        }

        logger.warn(logMessage)
    }

    private fun truncateText(text: String, maxLength: Int = 200): String {
        val escaped = text
            .replace("\n", "\\n")
            .replace("\r", "\\r")
            .replace("\t", "\\t")

        return if (escaped.length > maxLength) {
            "${escaped.take(maxLength)}... (${text.length} chars total)"
        } else {
            escaped
        }
    }
}
