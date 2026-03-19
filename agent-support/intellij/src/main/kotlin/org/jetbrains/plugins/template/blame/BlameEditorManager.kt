package org.jetbrains.plugins.template.blame

import com.intellij.openapi.Disposable
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.application.ReadAction
import com.intellij.openapi.components.Service
import com.intellij.openapi.diagnostic.thisLogger
import com.intellij.openapi.editor.Editor
import com.intellij.openapi.editor.EditorCustomElementRenderer
import com.intellij.openapi.editor.Inlay
import com.intellij.openapi.editor.colors.EditorFontType
import com.intellij.openapi.editor.event.CaretEvent
import com.intellij.openapi.editor.event.CaretListener
import com.intellij.openapi.editor.event.DocumentEvent
import com.intellij.openapi.editor.event.DocumentListener
import com.intellij.openapi.editor.markup.HighlighterLayer
import com.intellij.openapi.editor.markup.RangeHighlighter
import com.intellij.openapi.editor.markup.TextAttributes
import com.intellij.openapi.fileEditor.FileDocumentManager
import com.intellij.openapi.fileEditor.FileEditorManager
import com.intellij.openapi.fileEditor.FileEditorManagerListener
import com.intellij.openapi.fileEditor.TextEditor
import com.intellij.openapi.project.Project
import com.intellij.openapi.util.Disposer
import com.intellij.openapi.vfs.VirtualFile
import java.awt.Color
import java.awt.Graphics
import java.awt.Graphics2D
import java.awt.Rectangle
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.Executors
import java.util.concurrent.ScheduledExecutorService
import java.util.concurrent.ScheduledFuture
import java.util.concurrent.TimeUnit

/**
 * Project-level service that manages blame decorations across editors.
 * Renders gutter color stripes, inline after-text model annotations, and hover tooltips.
 */
@Service(Service.Level.PROJECT)
class BlameEditorManager(private val project: Project) : Disposable {

    private val logger = thisLogger()

    @Volatile
    var blameMode: BlameMode = BlameMode.LINE

    // Per-editor state
    private val editorStates = ConcurrentHashMap<Editor, EditorBlameState>()
    private val scheduler: ScheduledExecutorService = Executors.newSingleThreadScheduledExecutor { r ->
        Thread(r, "git-ai-blame-scheduler").apply { isDaemon = true }
    }

    // 40 distinct colors for prompt differentiation (same palette as VS Code extension)
    private val promptColors = listOf(
        Color(66, 133, 244),    // Blue
        Color(219, 68, 55),     // Red
        Color(244, 180, 0),     // Yellow
        Color(15, 157, 88),     // Green
        Color(171, 71, 188),    // Purple
        Color(255, 112, 67),    // Deep Orange
        Color(0, 172, 193),     // Cyan
        Color(124, 179, 66),    // Light Green
        Color(233, 30, 99),     // Pink
        Color(63, 81, 181),     // Indigo
        Color(255, 152, 0),     // Orange
        Color(0, 150, 136),     // Teal
        Color(156, 39, 176),    // Deep Purple
        Color(139, 195, 74),    // Lime
        Color(3, 169, 244),     // Light Blue
        Color(255, 87, 34),     // Burnt Orange
        Color(121, 85, 72),     // Brown
        Color(96, 125, 139),    // Blue Grey
        Color(244, 67, 54),     // Bright Red
        Color(33, 150, 243),    // Material Blue
        Color(76, 175, 80),     // Material Green
        Color(255, 193, 7),     // Amber
        Color(0, 188, 212),     // Material Cyan
        Color(103, 58, 183),    // Material Purple
        Color(205, 220, 57),    // Yellow Green
        Color(255, 138, 101),   // Light Orange
        Color(77, 208, 225),    // Light Cyan
        Color(186, 104, 200),   // Light Purple
        Color(174, 213, 129),   // Soft Green
        Color(100, 181, 246),   // Soft Blue
        Color(255, 183, 77),    // Soft Orange
        Color(128, 203, 196),   // Soft Teal
        Color(206, 147, 216),   // Soft Purple
        Color(220, 231, 117),   // Soft Lime
        Color(129, 212, 250),   // Pastel Blue
        Color(255, 171, 145),   // Pastel Orange
        Color(128, 222, 234),   // Pastel Cyan
        Color(179, 157, 219),   // Pastel Purple
        Color(165, 214, 167),   // Pastel Green
        Color(239, 154, 154)    // Pastel Red
    )

    companion object {
        private const val DEBOUNCE_MS = 300L
        fun getInstance(project: Project): BlameEditorManager =
            project.getService(BlameEditorManager::class.java)
    }

    /**
     * Initialize the manager: listen for editor events and decorate open editors.
     */
    fun initialize() {
        val connection = project.messageBus.connect(this)

        // Listen for file open/close events
        connection.subscribe(FileEditorManagerListener.FILE_EDITOR_MANAGER, object : FileEditorManagerListener {
            override fun fileOpened(source: FileEditorManager, file: VirtualFile) {
                source.getEditors(file).filterIsInstance<TextEditor>().forEach { textEditor ->
                    attachToEditor(textEditor.editor)
                }
            }

            override fun fileClosed(source: FileEditorManager, file: VirtualFile) {
                // Clean up editors for this file
                editorStates.keys.filter { editor ->
                    FileDocumentManager.getInstance().getFile(editor.document) == file
                }.forEach { detachFromEditor(it) }
            }
        })

        // Attach to already-open editors
        FileEditorManager.getInstance(project).allEditors.filterIsInstance<TextEditor>().forEach { textEditor ->
            attachToEditor(textEditor.editor)
        }
    }

    private fun attachToEditor(editor: Editor) {
        if (editorStates.containsKey(editor)) return

        val state = EditorBlameState(editor)
        editorStates[editor] = state

        // Listen for caret changes (for LINE mode)
        editor.caretModel.addCaretListener(object : CaretListener {
            override fun caretPositionChanged(event: CaretEvent) {
                if (blameMode == BlameMode.LINE) {
                    ApplicationManager.getApplication().invokeLater {
                        updateLineMode(editor, state)
                    }
                }
            }
        }, state)

        // Listen for document changes (re-fetch blame after edits)
        editor.document.addDocumentListener(object : DocumentListener {
            override fun documentChanged(event: DocumentEvent) {
                scheduleBlameRefresh(editor, state)
            }
        }, state)

        // Initial blame fetch
        scheduleBlameRefresh(editor, state)
    }

    private fun detachFromEditor(editor: Editor) {
        val state = editorStates.remove(editor) ?: return
        // Dispose on EDT to safely clear editor decorations.
        // Disposer.dispose calls state.dispose() -> clearDecorations().
        ApplicationManager.getApplication().invokeLater {
            Disposer.dispose(state)
        }
    }

    private fun scheduleBlameRefresh(editor: Editor, state: EditorBlameState) {
        state.pendingRefresh?.cancel(false)
        state.pendingRefresh = scheduler.schedule({
            fetchAndDecorate(editor, state)
        }, DEBOUNCE_MS, TimeUnit.MILLISECONDS)
    }

    private fun fetchAndDecorate(editor: Editor, state: EditorBlameState) {
        if (blameMode == BlameMode.OFF) {
            ApplicationManager.getApplication().invokeLater {
                state.clearDecorations()
            }
            return
        }

        // Read document content on the read thread
        val (file, content) = ReadAction.compute<Pair<VirtualFile?, String>, RuntimeException> {
            val f = FileDocumentManager.getInstance().getFile(editor.document)
            val c = editor.document.text
            f to c
        }
        if (file == null) return

        val blameService = BlameService.getInstance(project)
        val result = blameService.getBlame(file, content)

        ApplicationManager.getApplication().invokeLater {
            if (editor.isDisposed) return@invokeLater
            state.blameResult = result
            state.clearDecorations()
            if (result != null) {
                when (blameMode) {
                    BlameMode.ALL -> decorateAllMode(editor, state, result)
                    BlameMode.LINE -> updateLineMode(editor, state)
                    BlameMode.OFF -> {}
                }
            }
        }
    }

    /**
     * ALL mode: Show gutter stripes for all AI-authored lines, each prompt in a different color.
     */
    private fun decorateAllMode(editor: Editor, state: EditorBlameState, result: BlameResult) {
        val markupModel = editor.markupModel

        // Group lines by prompt hash for consistent coloring
        val linesByPrompt = result.lineAuthors.entries.groupBy { it.value.promptHash }

        for ((promptHash, entries) in linesByPrompt) {
            val color = colorForPromptHash(promptHash)
            val promptRecord = entries.firstOrNull()?.value?.promptRecord

            for ((lineNum, _) in entries) {
                val docLine = lineNum - 1 // blame uses 1-indexed lines
                if (docLine < 0 || docLine >= editor.document.lineCount) continue

                // Gutter stripe
                val highlighter = markupModel.addLineHighlighter(
                    docLine,
                    HighlighterLayer.SELECTION - 1,
                    null
                )
                highlighter.gutterIconRenderer = BlameGutterIcon(color, promptHash)
                state.highlighters.add(highlighter)
            }

            // Add inline after-text for last line of each contiguous range
            if (promptRecord != null) {
                val sortedLines = entries.map { it.key }.sorted()
                val lastLine = sortedLines.last() - 1
                if (lastLine >= 0 && lastLine < editor.document.lineCount) {
                    addInlineAnnotation(editor, state, lastLine, promptRecord, color)
                }
            }
        }
    }

    /**
     * LINE mode: Highlight all lines belonging to the same prompt as the current cursor line.
     */
    private fun updateLineMode(editor: Editor, state: EditorBlameState) {
        state.clearDecorations()
        val result = state.blameResult ?: return

        val currentLine = editor.caretModel.logicalPosition.line + 1 // 1-indexed
        val info = result.lineAuthors[currentLine]

        if (info != null) {
            // Highlight all lines from the same prompt
            val color = colorForPromptHash(info.promptHash)
            val samePromptLines = result.lineAuthors.filter { it.value.promptHash == info.promptHash }

            val markupModel = editor.markupModel
            for ((lineNum, _) in samePromptLines) {
                val docLine = lineNum - 1
                if (docLine < 0 || docLine >= editor.document.lineCount) continue

                val highlighter = markupModel.addLineHighlighter(
                    docLine,
                    HighlighterLayer.SELECTION - 1,
                    null
                )
                highlighter.gutterIconRenderer = BlameGutterIcon(color, info.promptHash)
                state.highlighters.add(highlighter)
            }

            // Add inline annotation on current line
            addInlineAnnotation(editor, state, currentLine - 1, info.promptRecord, color)
        }

        // Notify status bar to update
        BlameStatusBarWidgetFactory.update(project)
    }

    private fun addInlineAnnotation(
        editor: Editor,
        state: EditorBlameState,
        docLine: Int,
        promptRecord: PromptRecord,
        color: Color
    ) {
        val modelName = ModelNameParser.extractModelName(promptRecord.agentId?.model) ?: "AI"
        val toolName = promptRecord.agentId?.tool?.replaceFirstChar { it.uppercase() } ?: ""
        val displayText = if (toolName.isNotEmpty()) " $modelName via $toolName" else " $modelName"

        val renderer = InlineBlameRenderer(displayText, color, promptRecord)
        val offset = editor.document.getLineEndOffset(docLine)
        val inlay = editor.inlayModel.addAfterLineEndElement(offset, false, renderer)
        if (inlay != null) {
            state.inlays.add(inlay)
        }
    }

    /**
     * Get the blame info for the current cursor line (used by status bar widget).
     */
    fun getCurrentLineInfo(): Pair<LineBlameInfo?, BlameMode> {
        val editor = FileEditorManager.getInstance(project).selectedTextEditor ?: return null to blameMode
        val state = editorStates[editor] ?: return null to blameMode
        val result = state.blameResult ?: return null to blameMode
        val currentLine = editor.caretModel.logicalPosition.line + 1
        return result.lineAuthors[currentLine] to blameMode
    }

    /**
     * Cycle blame mode: OFF -> LINE -> ALL -> OFF
     */
    fun toggleBlameMode() {
        blameMode = when (blameMode) {
            BlameMode.OFF -> BlameMode.LINE
            BlameMode.LINE -> BlameMode.ALL
            BlameMode.ALL -> BlameMode.OFF
        }

        // Refresh all editors
        for ((editor, state) in editorStates) {
            if (!editor.isDisposed) {
                scheduleBlameRefresh(editor, state)
            }
        }

        BlameStatusBarWidgetFactory.update(project)
    }

    private fun colorForPromptHash(hash: String): Color {
        val index = (hash.hashCode() and Int.MAX_VALUE) % promptColors.size
        return promptColors[index]
    }

    override fun dispose() {
        scheduler.shutdownNow()
        val states = editorStates.values.toList()
        editorStates.clear()
        // Dispose on EDT to safely clear editor decorations (markup/inlay access requires EDT).
        ApplicationManager.getApplication().invokeLater {
            for (state in states) {
                Disposer.dispose(state)
            }
        }
    }

    /**
     * Tracks per-editor decoration state.
     */
    private class EditorBlameState(val editor: Editor) : Disposable {
        @Volatile
        var pendingRefresh: ScheduledFuture<*>? = null
        var blameResult: BlameResult? = null
        val highlighters = mutableListOf<RangeHighlighter>()
        val inlays = mutableListOf<Inlay<*>>()

        fun clearDecorations() {
            for (h in highlighters) {
                if (h.isValid) editor.markupModel.removeHighlighter(h)
            }
            highlighters.clear()

            for (inlay in inlays) {
                Disposer.dispose(inlay)
            }
            inlays.clear()
        }

        override fun dispose() {
            pendingRefresh?.cancel(false)
            clearDecorations()
        }
    }
}

/**
 * Renders a small colored stripe in the gutter, indicating AI authorship.
 */
private class BlameGutterIcon(
    private val color: Color,
    private val promptHash: String
) : com.intellij.openapi.editor.markup.GutterIconRenderer() {

    override fun getIcon(): javax.swing.Icon {
        return object : javax.swing.Icon {
            override fun paintIcon(c: java.awt.Component?, g: java.awt.Graphics, x: Int, y: Int) {
                val g2 = g as? Graphics2D ?: return
                g2.color = color
                g2.fillRect(x, y, iconWidth, iconHeight)
            }

            override fun getIconWidth(): Int = 4
            override fun getIconHeight(): Int = 16
        }
    }

    override fun getAlignment(): Alignment = Alignment.LEFT

    override fun getTooltipText(): String = "AI-authored code"

    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is BlameGutterIcon) return false
        return promptHash == other.promptHash && color == other.color
    }

    override fun hashCode(): Int = promptHash.hashCode() * 31 + color.hashCode()
}

/**
 * Renders inline text after the line end showing the AI model name.
 */
private class InlineBlameRenderer(
    private val text: String,
    private val color: Color,
    private val promptRecord: PromptRecord
) : EditorCustomElementRenderer {

    override fun calcWidthInPixels(inlay: Inlay<*>): Int {
        val editor = inlay.editor
        val fontMetrics = editor.contentComponent.getFontMetrics(
            editor.colorsScheme.getFont(EditorFontType.ITALIC)
        )
        return fontMetrics.stringWidth(text) + 8
    }

    override fun paint(inlay: Inlay<*>, g: Graphics, targetRegion: Rectangle, textAttributes: TextAttributes) {
        val g2 = g as? Graphics2D ?: return
        val editor = inlay.editor
        val font = editor.colorsScheme.getFont(EditorFontType.ITALIC)
        g2.font = font

        // Use the prompt color with some transparency
        g2.color = Color(color.red, color.green, color.blue, 180)

        val fontMetrics = g2.fontMetrics
        val y = targetRegion.y + targetRegion.height - fontMetrics.descent
        g2.drawString(text, targetRegion.x + 4, y)
    }

    override fun toString(): String = "InlineBlame($text)"
}
