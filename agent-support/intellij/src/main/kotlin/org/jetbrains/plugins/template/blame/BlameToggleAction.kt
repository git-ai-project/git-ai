package org.jetbrains.plugins.template.blame

import com.intellij.openapi.actionSystem.AnAction
import com.intellij.openapi.actionSystem.AnActionEvent

/**
 * Action to toggle git-ai blame mode (Off -> Line -> All -> Off).
 * Registered with keyboard shortcut Ctrl+Shift+A (Cmd+Shift+A on macOS).
 */
class BlameToggleAction : AnAction("Toggle Git AI Blame") {

    override fun actionPerformed(e: AnActionEvent) {
        val project = e.project ?: return
        val manager = BlameEditorManager.getInstance(project)
        manager.toggleBlameMode()
    }

    override fun update(e: AnActionEvent) {
        e.presentation.isEnabledAndVisible = e.project != null
        val project = e.project
        if (project != null) {
            val mode = BlameEditorManager.getInstance(project).blameMode
            e.presentation.text = "Toggle Git AI Blame (current: ${mode.name.lowercase()})"
        }
    }
}
