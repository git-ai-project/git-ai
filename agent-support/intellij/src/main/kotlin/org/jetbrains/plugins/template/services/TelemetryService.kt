package org.jetbrains.plugins.template.services

import com.intellij.ide.plugins.PluginManagerCore
import com.intellij.openapi.Disposable
import com.intellij.openapi.application.ApplicationInfo
import com.intellij.openapi.components.Service
import com.intellij.openapi.components.service
import com.intellij.openapi.diagnostic.thisLogger
import com.intellij.openapi.extensions.PluginId
import com.posthog.java.PostHog as PostHogClient
import io.sentry.Hint
import io.sentry.Sentry
import io.sentry.SentryEvent
import io.sentry.SentryLevel
import io.sentry.SentryOptions
import io.sentry.protocol.Message
import java.io.File

/**
 * Application-level service for analytics (PostHog) and error reporting (Sentry).
 * Matches the VS Code extension telemetry pattern.
 */
@Service(Service.Level.APP)
class TelemetryService : Disposable {

    private val logger = thisLogger()

    private var posthog: PostHogClient? = null
    private var distinctId: String? = null
    private var sentryInitialized = false

    companion object {
        private const val POSTHOG_API_KEY = "phc_XANaHNpDXBERPosyM8Bp0INVoGsgW8Gk92HsB090r6A"
        private const val POSTHOG_HOST = "https://us.i.posthog.com"
        private const val SENTRY_DSN = "https://8316e787580a70ad21e7158027caf849@o4510273204649984.ingest.us.sentry.io/4510812879060992"
        private const val PLUGIN_ID = "com.usegitai.plugins.jetbrains"

        /**
         * Package prefixes used to identify events originating from our plugin.
         * Events with stack frames matching these prefixes are sent to Sentry;
         * all other JVM exceptions (e.g. JetBrains internal errors) are dropped.
         */
        private val PLUGIN_PACKAGE_PREFIXES = listOf(
            "org.jetbrains.plugins.template",
            "com.usegitai"
        )

        /** Tag key set on events we create directly (not from uncaught exceptions). */
        private const val PLUGIN_EVENT_TAG = "git_ai_plugin_event"

        fun getInstance(): TelemetryService = service()
    }

    init {
        initializePostHog()
        initializeSentry()
    }

    private fun initializePostHog() {
        try {
            distinctId = readDistinctId()
            if (distinctId == null) {
                logger.info("No distinct_id found, PostHog analytics disabled")
                return
            }

            posthog = PostHogClient.Builder(POSTHOG_API_KEY)
                .host(POSTHOG_HOST)
                .build()

            logger.info("PostHog initialized with distinct_id: $distinctId")
        } catch (e: Exception) {
            logger.warn("Failed to initialize PostHog: ${e.message}")
        }
    }

    private fun initializeSentry() {
        try {
            Sentry.init { options ->
                options.dsn = SENTRY_DSN
                options.release = getPluginVersion()
                options.environment = "production"
                options.tracesSampleRate = 0.3
                options.setTag("ide", "intellij")
                options.setTag("ide_version", ApplicationInfo.getInstance().fullVersion)

                // Filter out events that don't originate from our plugin.
                // Since the plugin runs in the same JVM as the IDE, Sentry would
                // otherwise capture JetBrains internal errors as well.
                options.beforeSend = SentryOptions.BeforeSendCallback { event: SentryEvent, _: Hint ->
                    if (isPluginEvent(event)) event else null
                }
            }
            sentryInitialized = true
            logger.info("Sentry initialized")
        } catch (e: Exception) {
            logger.warn("Failed to initialize Sentry: ${e.message}")
        }
    }

    /**
     * Returns true if the event was explicitly sent by our plugin (tagged) or if
     * any exception in the event has a stack frame from our plugin packages.
     */
    private fun isPluginEvent(event: SentryEvent): Boolean {
        // Events we send ourselves are tagged
        if (event.getTag(PLUGIN_EVENT_TAG) != null) return true

        // Check exception stack traces for our package prefixes
        val exceptions = event.exceptions ?: return false
        return exceptions.any { exception ->
            exception.stacktrace?.frames?.any { frame ->
                val module = frame.module ?: return@any false
                PLUGIN_PACKAGE_PREFIXES.any { prefix -> module.startsWith(prefix) }
            } ?: false
        }
    }

    private fun readDistinctId(): String? {
        return try {
            val homeDir = System.getProperty("user.home")
            val distinctIdFile = File(homeDir, ".git-ai/internal/distinct_id")
            if (distinctIdFile.exists()) {
                distinctIdFile.readText().trim().takeIf { it.isNotEmpty() }
            } else {
                null
            }
        } catch (e: Exception) {
            logger.warn("Failed to read distinct_id: ${e.message}")
            null
        }
    }

    private fun getPluginVersion(): String {
        return try {
            // Use reflection to call PluginId.getId() to avoid Kotlin companion object
            // bytecode that doesn't exist in older IntelliJ versions (pre-252)
            val pluginIdClass = Class.forName("com.intellij.openapi.extensions.PluginId")
            val getIdMethod = pluginIdClass.getMethod("getId", String::class.java)
            val pluginId = getIdMethod.invoke(null, PLUGIN_ID) as? PluginId
            pluginId?.let { PluginManagerCore.getPlugin(it)?.version } ?: "unknown"
        } catch (e: Exception) {
            "unknown"
        }
    }

    private fun getCommonProperties(): Map<String, Any> {
        return mapOf(
            "ide" to "intellij",
            "ide_version" to ApplicationInfo.getInstance().fullVersion,
            "ide_build" to ApplicationInfo.getInstance().build.asString(),
            "plugin_version" to getPluginVersion(),
            "os" to (System.getProperty("os.name") ?: "unknown"),
            "arch" to (System.getProperty("os.arch") ?: "unknown")
        )
    }

    /**
     * Captures the plugin startup event.
     */
    fun captureStartupEvent() {
        captureEvent("intellij_plugin_startup", getCommonProperties())
    }

    /**
     * Reports that git-ai CLI was not found (simple version for backwards compatibility).
     */
    fun reportGitAiNotFound() {
        reportGitAiNotFound(null, null, emptyList(), null)
    }

    /**
     * Reports that git-ai CLI was not found with detailed context.
     */
    fun reportGitAiNotFound(
        exitCode: Int?,
        output: String?,
        searchedPaths: List<String>,
        currentPath: String?
    ) {
        val context = mutableMapOf<String, Any>(
            "error_type" to "git_ai_not_found"
        )
        exitCode?.let { context["exit_code"] = it }
        output?.let { context["output"] = it.take(500) }
        if (searchedPaths.isNotEmpty()) {
            context["searched_paths"] = searchedPaths.joinToString(",")
        }
        currentPath?.let { context["path_env"] = it.take(500) }

        captureEvent("git_ai_error", getCommonProperties() + context)

        val message = buildString {
            append("git-ai CLI not found")
            exitCode?.let { append(" (exit code: $it)") }
            if (searchedPaths.isNotEmpty()) {
                append(". Searched: ${searchedPaths.joinToString(", ")}")
            }
        }
        captureSentryMessage(message, SentryLevel.WARNING, context)
    }

    /**
     * Reports a version mismatch where git-ai is below minimum required version.
     */
    fun reportVersionMismatch(foundVersion: String, requiredVersion: String) {
        val context = mapOf(
            "error_type" to "version_mismatch",
            "found_version" to foundVersion,
            "required_version" to requiredVersion
        )
        captureEvent("git_ai_error", getCommonProperties() + context)
        captureSentryMessage(
            "git-ai version mismatch: found $foundVersion, required $requiredVersion",
            SentryLevel.WARNING,
            context
        )
    }

    /**
     * Reports a checkpoint failure.
     */
    fun reportCheckpointFailure(exitCode: Int, output: String) {
        val context = mapOf(
            "error_type" to "checkpoint_failure",
            "exit_code" to exitCode.toString(),
            "output" to output.take(500) // Limit output size
        )
        captureEvent("git_ai_error", getCommonProperties() + context)
        captureSentryMessage(
            "git-ai checkpoint failed with exit code $exitCode",
            SentryLevel.ERROR,
            context
        )
    }

    /**
     * Reports a checkpoint timeout (exceeded 30s).
     */
    fun reportCheckpointTimeout() {
        val context = mapOf("error_type" to "checkpoint_timeout")
        captureEvent("git_ai_error", getCommonProperties() + context)
        captureSentryMessage("git-ai checkpoint timed out after 30 seconds", SentryLevel.ERROR, context)
    }

    /**
     * Captures a general error/exception.
     */
    fun captureError(throwable: Throwable, context: Map<String, String> = emptyMap()) {
        val eventContext = mapOf("error_type" to "exception") + context
        captureEvent("git_ai_error", getCommonProperties() + eventContext)

        if (sentryInitialized) {
            try {
                Sentry.captureException(throwable) { scope ->
                    scope.setTag(PLUGIN_EVENT_TAG, "true")
                    context.forEach { (key, value) ->
                        scope.setExtra(key, value)
                    }
                }
            } catch (e: Exception) {
                logger.warn("Failed to capture exception in Sentry: ${e.message}")
            }
        }
    }

    private fun captureEvent(eventName: String, properties: Map<String, Any>) {
        val ph = posthog ?: return
        val id = distinctId ?: return

        try {
            ph.capture(id, eventName, properties)
        } catch (e: Exception) {
            logger.warn("Failed to capture PostHog event: ${e.message}")
        }
    }

    private fun captureSentryMessage(message: String, level: SentryLevel, context: Map<String, Any>) {
        if (!sentryInitialized) return

        try {
            val event = SentryEvent().apply {
                this.message = Message().apply { this.message = message }
                this.level = level
                setTag(PLUGIN_EVENT_TAG, "true")
                context.forEach { (key, value) ->
                    setExtra(key, value)
                }
                // Add distinct_id if available
                distinctId?.let { setExtra("distinct_id", it) }
            }
            Sentry.captureEvent(event)
        } catch (e: Exception) {
            logger.warn("Failed to capture Sentry message: ${e.message}")
        }
    }

    override fun dispose() {
        try {
            posthog?.shutdown()
            logger.info("PostHog shut down")
        } catch (e: Exception) {
            logger.warn("Error shutting down PostHog: ${e.message}")
        }

        try {
            if (sentryInitialized) {
                Sentry.close()
                logger.info("Sentry closed")
            }
        } catch (e: Exception) {
            logger.warn("Error closing Sentry: ${e.message}")
        }
    }
}
