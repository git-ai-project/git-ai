package org.jetbrains.plugins.template.services

import com.intellij.openapi.components.Service
import com.intellij.openapi.components.service
import com.intellij.openapi.diagnostic.thisLogger
import org.jetbrains.plugins.template.model.AgentV1Input
import java.io.File
import java.util.concurrent.TimeUnit

/**
 * Application-level service that interacts with the git-ai CLI
 * to create checkpoints when AI agents make edits.
 */
@Service(Service.Level.APP)
class GitAiService {

    private val logger = thisLogger()
    private val minVersion = Version(1, 0, 23)

    @Volatile
    private var availabilityChecked = false

    @Volatile
    private var isAvailable = false

    @Volatile
    private var cachedVersion: Version? = null

    data class Version(val major: Int, val minor: Int, val patch: Int) : Comparable<Version> {
        override fun compareTo(other: Version): Int {
            return compareValuesBy(this, other, { it.major }, { it.minor }, { it.patch })
        }

        override fun toString(): String = "$major.$minor.$patch"

        companion object {
            fun parse(versionString: String): Version? {
                // Expected format: "1.0.39 (debug)" or "1.0.39"
                val versionPart = versionString.trim().split(" ").first()

                val parts = versionPart.split(".")
                if (parts.size < 3) return null

                return try {
                    Version(
                        parts[0].toInt(),
                        parts[1].toInt(),
                        parts[2].split("-", "+").first().toInt()
                    )
                } catch (e: NumberFormatException) {
                    null
                }
            }
        }
    }

    /**
     * Checks if git-ai CLI is installed and meets the minimum version requirement.
     */
    fun checkAvailable(): Boolean {
        if (availabilityChecked) {
            return isAvailable
        }

        synchronized(this) {
            if (availabilityChecked) {
                return isAvailable
            }

            isAvailable = checkGitAiInstalled()
            availabilityChecked = true
            return isAvailable
        }
    }

    private fun checkGitAiInstalled(): Boolean {
        return try {
            val process = ProcessBuilder("git-ai", "version")
                .redirectErrorStream(true)
                .start()

            val completed = process.waitFor(5, TimeUnit.SECONDS)
            if (!completed) {
                process.destroyForcibly()
                logger.warn("git-ai version check timed out")
                return false
            }

            if (process.exitValue() != 0) {
                logger.warn("git-ai not found or returned error")
                return false
            }

            val output = process.inputStream.bufferedReader().readText().trim()
            val version = Version.parse(output)

            if (version == null) {
                logger.warn("Could not parse git-ai version from: $output")
                return false
            }

            cachedVersion = version

            if (version < minVersion) {
                logger.warn("git-ai version $version is below minimum required version $minVersion")
                return false
            }

            logger.warn("git-ai CLI available, version: $version")
            true
        } catch (e: Exception) {
            logger.warn("git-ai CLI not available: ${e.message}")
            false
        }
    }

    /**
     * Creates a checkpoint by calling git-ai checkpoint agent-v1 command.
     *
     * @param input The checkpoint data to send via stdin (Human or AiAgent)
     * @param workingDirectory The working directory (git repo root) for the command
     * @return true if checkpoint was created successfully
     */
    fun checkpoint(input: AgentV1Input, workingDirectory: String): Boolean {
        if (!checkAvailable()) {
            logger.warn("Skipping checkpoint - git-ai not available")
            return false
        }

        return try {
            val jsonInput = input.toJson()
            val inputType = when (input) {
                is AgentV1Input.Human -> "human"
                is AgentV1Input.AiAgent -> "ai_agent (${input.agentName})"
            }

            logger.warn("Creating checkpoint (agent-v1): $inputType")
            logger.warn("Checkpoint input: $jsonInput")

            val process = ProcessBuilder(
                "git-ai",
                "checkpoint",
                "agent-v1",
                "--hook-input",
                "stdin"
            )
                .directory(File(workingDirectory))
                .redirectErrorStream(true)
                .start()

            // Write JSON to stdin
            process.outputStream.bufferedWriter().use { writer ->
                writer.write(jsonInput)
            }

            val completed = process.waitFor(30, TimeUnit.SECONDS)
            if (!completed) {
                process.destroyForcibly()
                logger.warn("git-ai checkpoint timed out")
                return false
            }

            val output = process.inputStream.bufferedReader().readText().trim()
            val exitCode = process.exitValue()

            if (exitCode != 0) {
                logger.warn("git-ai checkpoint failed (exit $exitCode): $output")
                return false
            }

            logger.warn("Checkpoint created successfully ($inputType)")
            if (output.isNotEmpty()) {
                logger.warn("git-ai output: $output")
            }
            true
        } catch (e: Exception) {
            logger.warn("Failed to create checkpoint: ${e.message}", e)
            false
        }
    }

    /**
     * Resets the availability check, forcing a re-check on next call.
     * Useful if the user installs git-ai during the session.
     */
    fun resetAvailabilityCheck() {
        synchronized(this) {
            availabilityChecked = false
            cachedVersion = null
        }
    }

    companion object {
        fun getInstance(): GitAiService = service()
    }
}
