package org.jetbrains.plugins.template.blame

import com.intellij.openapi.project.Project
import com.intellij.openapi.startup.ProjectActivity

/**
 * Initializes the blame editor manager when a project is opened.
 */
class BlameStartupActivity : ProjectActivity {
    override suspend fun execute(project: Project) {
        BlameEditorManager.getInstance(project).initialize()
    }
}
