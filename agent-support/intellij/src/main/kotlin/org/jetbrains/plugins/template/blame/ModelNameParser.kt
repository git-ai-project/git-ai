package org.jetbrains.plugins.template.blame

/**
 * Parses raw model strings into human-readable display names.
 * Ported from the VS Code extension's extractModelName logic.
 */
object ModelNameParser {

    fun extractModelName(modelString: String?): String? {
        if (modelString.isNullOrBlank()) return null

        val trimmed = modelString.trim().lowercase()

        if (trimmed == "default" || trimmed == "auto") return "Cursor"
        if (trimmed == "unknown") return null

        val parts = modelString.lowercase().split("-").filter { p ->
            // Skip date suffixes (8+ digits)
            if (p.matches(Regex("^\\d{8,}$"))) return@filter false
            // Skip "thinking" variant
            if (p == "thinking") return@filter false
            true
        }

        if (parts.isEmpty()) return modelString.trim()

        // GPT models: gpt-4o -> GPT 4o, gpt-4o-mini -> GPT 4o Mini
        if (parts[0] == "gpt") {
            val rest = parts.drop(1)
            if (rest.isEmpty()) return "GPT"
            val variant = rest.mapIndexed { i, p ->
                if (i == 0) p else p.replaceFirstChar { it.uppercase() }
            }.joinToString(" ")
            return "GPT $variant"
        }

        // Claude models: claude-3-5-sonnet -> Sonnet 3.5, claude-opus-4 -> Opus 4
        if (parts[0] == "claude") {
            val rest = parts.drop(1)
            val modelNames = listOf("opus", "sonnet", "haiku")
            var modelName = ""
            val versions = mutableListOf<String>()

            for (p in rest) {
                if (p in modelNames) {
                    modelName = p.replaceFirstChar { it.uppercase() }
                } else if (p.matches(Regex("^[\\d.]+$"))) {
                    versions.add(p)
                }
            }

            if (modelName.isNotEmpty()) {
                val versionStr = versions.joinToString(".")
                return if (versionStr.isNotEmpty()) "$modelName $versionStr" else modelName
            }
            return "Claude"
        }

        // Gemini models: gemini-1.5-flash -> Gemini Flash 1.5
        if (parts[0] == "gemini") {
            val rest = parts.drop(1)
            val variantNames = listOf("pro", "flash", "ultra", "nano")
            var variantName = ""
            var version = ""

            for (p in rest) {
                if (p in variantNames) {
                    variantName = p.replaceFirstChar { it.uppercase() }
                } else if (p.matches(Regex("^[\\d.]+$"))) {
                    version = p
                }
            }

            return when {
                variantName.isNotEmpty() && version.isNotEmpty() -> "Gemini $variantName $version"
                variantName.isNotEmpty() -> "Gemini $variantName"
                version.isNotEmpty() -> "Gemini $version"
                else -> "Gemini"
            }
        }

        // o1, o3, o4-mini models: o1 -> O1, o3-mini -> O3 Mini
        if (parts[0].matches(Regex("^o\\d"))) {
            return parts.joinToString(" ") { p ->
                if (p.matches(Regex("^o\\d"))) p.uppercase()
                else p.replaceFirstChar { it.uppercase() }
            }
        }

        // Codex models: codex-5.2 -> Codex 5.2
        if (parts[0] == "codex") {
            val version = parts.find { it.matches(Regex("^[\\d.]+$")) }
            return if (version != null) "Codex $version" else "Codex"
        }

        return modelString.trim()
    }
}
