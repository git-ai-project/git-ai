package org.jetbrains.plugins.template.services

import com.intellij.openapi.Disposable
import com.intellij.openapi.components.Service
import com.intellij.openapi.diagnostic.thisLogger
import com.intellij.openapi.editor.EditorFactory
import org.jetbrains.plugins.template.listener.DocumentChangeListener

/**
 * Application-level service that registers the DocumentChangeListener to track
 * document changes and identify AI agent plugins that triggered them.
 */
@Service(Service.Level.APP)
class DocumentChangeTrackerService : Disposable {

    init {
        thisLogger().warn("DocumentChangeTrackerService initializing...")

        val listener = DocumentChangeListener()
        EditorFactory.getInstance().eventMulticaster.addDocumentListener(listener, this)

        thisLogger().warn("DocumentChangeTrackerService initialized - now tracking document changes")
    }

    override fun dispose() {
        thisLogger().warn("DocumentChangeTrackerService disposed")
    }
}
