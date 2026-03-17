package org.jetbrains.plugins.template.blame

import com.google.gson.Gson
import com.intellij.openapi.components.Service
import com.intellij.openapi.components.service
import com.intellij.openapi.diagnostic.thisLogger
import com.intellij.openapi.project.Project
import com.intellij.openapi.vfs.VirtualFile
import org.jetbrains.plugins.template.services.GitAiService
import java.io.File
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.TimeUnit

/**
 * Project-level service that executes `git-ai blame --json` and caches results.
 */
@Service(Service.Level.PROJECT)
class BlameService(private val project: Project) {

    private val logger = thisLogger()
    private val gson = Gson()

    private val cache = ConcurrentHashMap<String, BlameResult>()

    companion object {
        private const val TIMEOUT_MS = 30_000L
        private const val MAX_CACHE_ENTRIES = 20

        fun getInstance(project: Project): BlameService = project.service()
    }

    /**
     * Get blame for a file. Returns cached result if available, otherwise runs the CLI.
     */
    fun getBlame(file: VirtualFile, content: String): BlameResult? {
        val filePath = file.path
        val cacheKey = "$filePath:${content.hashCode()}"

        cache[cacheKey]?.let { return it }

        val result = runBlame(filePath, content) ?: return null

        // Evict oldest entries if cache is full
        if (cache.size >= MAX_CACHE_ENTRIES) {
            val oldest = cache.entries.minByOrNull { it.value.timestamp }
            oldest?.let { cache.remove(it.key) }
        }
        cache[cacheKey] = result
        return result
    }

    /**
     * Invalidate cache for a specific file.
     */
    fun invalidate(filePath: String) {
        cache.keys.removeAll { it.startsWith(filePath) }
    }

    /**
     * Clear entire cache.
     */
    fun clearCache() {
        cache.clear()
    }

    private fun runBlame(filePath: String, content: String): BlameResult? {
        val gitAiService = GitAiService.getInstance()
        if (!gitAiService.checkAvailable()) {
            logger.info("git-ai not available, skipping blame")
            return null
        }

        val gitAiPath = gitAiService.resolvedPath ?: return null

        // Find git repo root for this file
        val repoRoot = findGitRoot(filePath) ?: return null

        return try {
            val command = listOf(gitAiPath, "blame", "--json", "--contents", "-", filePath)
            val process = ProcessBuilder(command)
                .directory(File(repoRoot))
                .redirectErrorStream(false)
                .start()

            // Write current file content to stdin
            process.outputStream.bufferedWriter().use { writer ->
                writer.write(content)
            }

            val completed = process.waitFor(TIMEOUT_MS, TimeUnit.MILLISECONDS)
            if (!completed) {
                process.destroyForcibly()
                logger.warn("git-ai blame timed out for $filePath")
                return null
            }

            val stdout = process.inputStream.bufferedReader().readText().trim()
            val stderr = process.errorStream.bufferedReader().readText().trim()

            if (process.exitValue() != 0) {
                logger.info("git-ai blame failed for $filePath: $stderr")
                return null
            }

            if (stdout.isEmpty()) return null

            parseBlameOutput(stdout, content.lines().size)
        } catch (e: Exception) {
            logger.warn("Failed to run git-ai blame for $filePath: ${e.message}")
            null
        }
    }

    private fun parseBlameOutput(json: String, totalLines: Int): BlameResult? {
        return try {
            val output = gson.fromJson(json, BlameJsonOutput::class.java) ?: return null

            // Expand line ranges ("11-114" -> individual lines 11..114)
            val lineAuthors = mutableMapOf<Int, LineBlameInfo>()
            for ((rangeStr, promptHash) in output.lines) {
                val prompt = output.prompts[promptHash] ?: continue
                val info = LineBlameInfo(promptHash, prompt)

                if (rangeStr.contains("-")) {
                    val parts = rangeStr.split("-")
                    val start = parts[0].toIntOrNull() ?: continue
                    val end = parts[1].toIntOrNull() ?: continue
                    for (line in start..end) {
                        lineAuthors[line] = info
                    }
                } else {
                    val line = rangeStr.toIntOrNull() ?: continue
                    lineAuthors[line] = info
                }
            }

            BlameResult(
                lineAuthors = lineAuthors,
                prompts = output.prompts,
                metadata = output.metadata,
                timestamp = System.currentTimeMillis(),
                totalLines = totalLines
            )
        } catch (e: Exception) {
            logger.warn("Failed to parse blame JSON: ${e.message}")
            null
        }
    }

    private fun findGitRoot(filePath: String): String? {
        var dir = File(filePath).parentFile
        while (dir != null) {
            if (File(dir, ".git").exists()) return dir.absolutePath
            dir = dir.parentFile
        }
        return null
    }
}
