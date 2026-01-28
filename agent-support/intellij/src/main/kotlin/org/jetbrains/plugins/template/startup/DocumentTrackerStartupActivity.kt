package org.jetbrains.plugins.template.startup

import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.project.Project
import com.intellij.openapi.startup.ProjectActivity
import org.jetbrains.plugins.template.services.DocumentChangeTrackerService

/**
 * Startup activity that initializes the DocumentChangeTrackerService.
 * This is needed because preload="true" is ignored for non-core plugins.
 */
class DocumentTrackerStartupActivity : ProjectActivity {

    override suspend fun execute(project: Project) {
        // Request the service to trigger its initialization
        ApplicationManager.getApplication().getService(DocumentChangeTrackerService::class.java)
    }
}
