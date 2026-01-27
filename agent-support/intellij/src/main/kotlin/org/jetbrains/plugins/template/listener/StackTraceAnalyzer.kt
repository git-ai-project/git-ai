package org.jetbrains.plugins.template.listener

/**
 * Analyzes stack traces to detect which AI agent plugin triggered a document change.
 */
object StackTraceAnalyzer {

    enum class Confidence {
        HIGH,
        MEDIUM,
        LOW,
        NONE
    }

    data class AnalysisResult(
        val sourceName: String?,
        val confidence: Confidence,
        val relevantFrames: List<StackTraceElement>
    )

    private data class AgentPattern(
        val name: String,
        val packagePatterns: List<String>,
        val classPatterns: List<String> = emptyList()
    )

    private val knownAgents = listOf(
        AgentPattern(
            name = "GitHub Copilot",
            packagePatterns = listOf("com.github.copilot"),
            classPatterns = listOf("copilot")
        ),
        AgentPattern(
            name = "Augment Code",
            packagePatterns = listOf("com.augment", "co.augment"),
            classPatterns = listOf("augment")
        ),
        AgentPattern(
            name = "Tabnine",
            packagePatterns = listOf("com.tabnine"),
            classPatterns = listOf("tabnine")
        ),
        AgentPattern(
            name = "Codeium",
            packagePatterns = listOf("com.codeium"),
            classPatterns = listOf("codeium")
        ),
        AgentPattern(
            name = "AWS CodeWhisperer",
            packagePatterns = listOf("software.aws.toolkits", "software.amazon.awssdk"),
            classPatterns = listOf("codewhisperer", "amazonq")
        ),
        AgentPattern(
            name = "JetBrains AI Assistant",
            packagePatterns = listOf("com.intellij.ml", "com.jetbrains.ml"),
            classPatterns = listOf("aiassistant", "mlcode")
        ),
        AgentPattern(
            name = "Cursor",
            packagePatterns = listOf("com.cursor"),
            classPatterns = listOf("cursor")
        ),
        AgentPattern(
            name = "Sourcegraph Cody",
            packagePatterns = listOf("com.sourcegraph"),
            classPatterns = listOf("cody", "sourcegraph")
        ),
        AgentPattern(
            name = "Continue",
            packagePatterns = listOf("com.continue"),
            classPatterns = listOf("continue")
        )
    )

    fun analyze(stackTrace: Array<StackTraceElement>): AnalysisResult {
        val relevantFrames = mutableListOf<StackTraceElement>()
        var detectedAgent: String? = null
        var confidence = Confidence.NONE

        for (frame in stackTrace) {
            val className = frame.className.lowercase()
            val fullName = frame.className

            for (agent in knownAgents) {
                // Check package patterns (high confidence)
                val matchesPackage = agent.packagePatterns.any { pattern ->
                    fullName.startsWith(pattern, ignoreCase = true)
                }

                // Check class name patterns (medium confidence)
                val matchesClass = agent.classPatterns.any { pattern ->
                    className.contains(pattern)
                }

                if (matchesPackage) {
                    relevantFrames.add(frame)
                    if (detectedAgent == null) {
                        detectedAgent = agent.name
                        confidence = Confidence.HIGH
                    }
                } else if (matchesClass && detectedAgent == null) {
                    relevantFrames.add(frame)
                    detectedAgent = agent.name
                    confidence = Confidence.MEDIUM
                }
            }
        }

        // If no specific agent detected, check for generic patterns
        if (detectedAgent == null) {
            for (frame in stackTrace) {
                val className = frame.className.lowercase()
                if (className.contains("completion") ||
                    className.contains("inlay") ||
                    className.contains("inline") ||
                    className.contains("suggestion")
                ) {
                    relevantFrames.add(frame)
                    if (confidence == Confidence.NONE) {
                        confidence = Confidence.LOW
                        detectedAgent = "Unknown AI Assistant (generic pattern)"
                    }
                }
            }
        }

        return AnalysisResult(
            sourceName = detectedAgent,
            confidence = confidence,
            relevantFrames = relevantFrames
        )
    }

    fun formatStackTrace(stackTrace: Array<StackTraceElement>, maxFrames: Int = 50): String {
        return stackTrace.take(maxFrames).joinToString("\n") { frame ->
            "  at ${frame.className}.${frame.methodName}(${frame.fileName}:${frame.lineNumber})"
        }
    }

    fun formatRelevantFrames(frames: List<StackTraceElement>): String {
        if (frames.isEmpty()) return "  (no relevant frames detected)"
        return frames.joinToString("\n") { frame ->
            "  ${frame.className}.${frame.methodName}(${frame.fileName}:${frame.lineNumber})"
        }
    }
}
