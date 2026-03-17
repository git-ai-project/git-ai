package org.jetbrains.plugins.template.blame

import com.intellij.openapi.project.Project
import com.intellij.openapi.wm.StatusBar
import com.intellij.openapi.wm.StatusBarWidget
import com.intellij.openapi.wm.StatusBarWidgetFactory
import com.intellij.openapi.wm.WindowManager
import com.intellij.util.Consumer
import java.awt.Component
import java.awt.event.MouseEvent

/**
 * Status bar widget showing AI/human attribution for the current cursor line.
 * Clicking toggles blame mode.
 */
class BlameStatusBarWidgetFactory : StatusBarWidgetFactory {

    companion object {
        const val WIDGET_ID = "GitAiBlame"

        fun update(project: Project) {
            val statusBar = WindowManager.getInstance().getStatusBar(project) ?: return
            statusBar.updateWidget(WIDGET_ID)
        }
    }

    override fun getId(): String = WIDGET_ID

    override fun getDisplayName(): String = "Git AI Blame"

    override fun isAvailable(project: Project): Boolean = true

    override fun createWidget(project: Project): StatusBarWidget = BlameStatusBarWidget(project)

    override fun canBeEnabledOn(statusBar: StatusBar): Boolean = true
}

private class BlameStatusBarWidget(private val project: Project) : StatusBarWidget, StatusBarWidget.TextPresentation {

    private var statusBar: StatusBar? = null

    override fun ID(): String = BlameStatusBarWidgetFactory.WIDGET_ID

    override fun install(statusBar: StatusBar) {
        this.statusBar = statusBar
    }

    override fun getPresentation(): StatusBarWidget.WidgetPresentation = this

    override fun getText(): String {
        val manager = BlameEditorManager.getInstance(project)
        val (info, mode) = manager.getCurrentLineInfo()

        if (mode == BlameMode.OFF) return "AI Blame: Off"

        return if (info != null) {
            val modelName = ModelNameParser.extractModelName(info.promptRecord.agentId?.model) ?: "AI"
            "\uD83E\uDD16 $modelName"  // robot emoji
        } else {
            "\uD83E\uDDD1\u200D\uD83D\uDCBB Human"  // person with laptop emoji
        }
    }

    override fun getTooltipText(): String {
        val manager = BlameEditorManager.getInstance(project)
        val (info, mode) = manager.getCurrentLineInfo()

        return when {
            mode == BlameMode.OFF -> "Git AI Blame is off. Click to enable."
            info != null -> {
                val tool = info.promptRecord.agentId?.tool ?: "unknown"
                val model = info.promptRecord.agentId?.model ?: "unknown"
                val human = info.promptRecord.humanAuthor ?: "Unknown"
                "AI-authored by $human using $model via $tool\nClick to change blame mode"
            }
            else -> "Human-authored line. Click to change blame mode."
        }
    }

    override fun getAlignment(): Float = Component.RIGHT_ALIGNMENT

    override fun getClickConsumer(): Consumer<MouseEvent> = Consumer {
        BlameEditorManager.getInstance(project).toggleBlameMode()
    }

    override fun dispose() {
        statusBar = null
    }
}
