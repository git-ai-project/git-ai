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
        val name: String,  // Tool name for git-ai (lowercase with hyphens)
        val packagePatterns: List<String>,
        val classPatterns: List<String> = emptyList()
    )

    private val knownAgents = listOf(
        AgentPattern(
            name = "github-copilot-jetbrains",
            packagePatterns = listOf("com.github.copilot"),
            classPatterns = listOf("copilot")
        ),
        AgentPattern(
            name = "junie",
            packagePatterns = listOf("com.intellij.ml.llm.matterhorn.junie"),
            classPatterns = listOf("junie")
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
