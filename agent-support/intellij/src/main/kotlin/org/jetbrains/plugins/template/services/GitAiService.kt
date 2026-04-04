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

    // Stable session ID based on when the service was initialized
    val sessionId: String = System.currentTimeMillis().toString()

    @Volatile
    private var availabilityChecked = false

    @Volatile
    private var isAvailable = false

    @Volatile
    private var cachedVersion: Version? = null

    // Cached path to git-ai binary once resolved
    @Volatile
    private var resolvedGitAiPath: String? = null

    // Track which locations were searched (for error reporting)
    private var lastSearchedPaths: List<String> = emptyList()

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
     * Finds the git-ai binary by checking known installation locations first,
     * then falling back to PATH lookup.
     *
     * Known locations (from install.sh, install.ps1, scripts/dev.sh):
     * - Production/dev build: ~/.git-ai/bin/git-ai  (dev.sh installs here too)
     * - Nix development: ~/.git-ai-local-dev/gitwrap/bin/git-ai  (nix develop shellHook)
     *
     * @return The full path to git-ai if found, or null if not found
     */
    private fun findGitAiBinary(): String? {
        // Return cached path if already resolved and still valid
        resolvedGitAiPath?.let { path ->
            if (File(path).canExecute()) {
                return path
            }
            // Cached path no longer valid, clear it
            resolvedGitAiPath = null
        }

        val homeDir = System.getProperty("user.home")
        val isWindows = System.getProperty("os.name").lowercase().contains("win")

        // Known installation locations from install.sh/install.ps1/scripts/dev.sh
        // Nix dev path checked first so nix develop users can test local builds
        val knownPaths = if (isWindows) {
            listOf(
                "$homeDir\\.git-ai-local-dev\\gitwrap\\bin\\git-ai.exe",  // Nix dev (nix develop shellHook)
                "$homeDir\\.git-ai\\bin\\git-ai.exe"                      // Production + non-Nix dev (install.ps1 / dev.sh)
            )
        } else {
            listOf(
                "$homeDir/.git-ai-local-dev/gitwrap/bin/git-ai", // Nix dev (nix develop shellHook)
                "$homeDir/.git-ai/bin/git-ai"                    // Production + non-Nix dev (install.sh / dev.sh)
            )
        }

        lastSearchedPaths = knownPaths

        // Check known locations first
        for (path in knownPaths) {
            val file = File(path)
            if (file.exists() && file.canExecute()) {
                logger.info("Found git-ai at known location: $path")
                resolvedGitAiPath = path
                return path
            }
        }

        // Fall back to PATH lookup via shell (may work if launched from terminal)
        logger.info("git-ai not found in known locations, trying PATH lookup")
        return tryPathLookup()
    }

    /**
     * Attempts to find git-ai via PATH using the shell.
     * This may work when IntelliJ is launched from a terminal with proper PATH.
     */
    private fun tryPathLookup(): String? {
        return try {
            val isWindows = System.getProperty("os.name").lowercase().contains("win")
            val command = if (isWindows) {
                listOf("cmd", "/c", "where git-ai")
            } else {
                listOf("/bin/sh", "-l", "-c", "which git-ai")
            }

            val process = ProcessBuilder(command)
                .redirectErrorStream(true)
                .start()

            val completed = process.waitFor(5, TimeUnit.SECONDS)
            if (!completed) {
                process.destroyForcibly()
                return null
            }

            if (process.exitValue() == 0) {
                val path = process.inputStream.bufferedReader().readText().trim().lines().firstOrNull()
                if (path != null && File(path).canExecute()) {
                    logger.info("Found git-ai via PATH lookup: $path")
                    resolvedGitAiPath = path
                    return path
                }
            }
            null
        } catch (e: Exception) {
            logger.warn("PATH lookup for git-ai failed: ${e.message}")
            null
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
            // First, try to find the git-ai binary
            val gitAiPath = findGitAiBinary()

            if (gitAiPath == null) {
                val currentPath = System.getenv("PATH") ?: "PATH not set"
                logger.warn("""
                    git-ai not found
                    Searched locations: ${lastSearchedPaths.joinToString(", ")}
                    PATH: $currentPath

                    To fix: Install git-ai using one of these methods:
                    - cargo install git-ai
                    - curl -fsSL https://install.usegitai.com | sh
                    - Or ensure git-ai is in your PATH
                """.trimIndent())
                TelemetryService.getInstance().reportGitAiNotFound(
                    exitCode = null,
                    output = null,
                    searchedPaths = lastSearchedPaths,
                    currentPath = currentPath
                )
                return false
            }

            // Run version check using the resolved path
            val command = listOf(gitAiPath, "version")
            val process = ProcessBuilder(command)
                .redirectErrorStream(false)
                .start()

            val completed = process.waitFor(5, TimeUnit.SECONDS)
            if (!completed) {
                process.destroyForcibly()
                logger.warn("git-ai version check timed out")
                return false
            }

            val output = process.inputStream.bufferedReader().readText().trim()
            val errorOutput = process.errorStream.bufferedReader().readText().trim()

            if (process.exitValue() != 0) {
                val currentPath = System.getenv("PATH") ?: "PATH not set"
                logger.warn("""
                    git-ai returned error
                    Command: ${command.joinToString(" ")}
                    Exit code: ${process.exitValue()}
                    Stdout: $output
                    Stderr: $errorOutput
                    PATH: $currentPath
                    Resolved path: $gitAiPath
                """.trimIndent())
                TelemetryService.getInstance().reportGitAiNotFound(
                    exitCode = process.exitValue(),
                    output = if (errorOutput.isNotEmpty()) errorOutput else output,
                    searchedPaths = lastSearchedPaths,
                    currentPath = currentPath
                )
                return false
            }

            val version = Version.parse(output)

            if (version == null) {
                logger.warn("Could not parse git-ai version from: $output")
                return false
            }

            cachedVersion = version

            if (version < minVersion) {
                logger.warn("git-ai version $version is below minimum required version $minVersion")
                TelemetryService.getInstance().reportVersionMismatch(version.toString(), minVersion.toString())
                return false
            }

            logger.info("git-ai CLI available at $gitAiPath, version: $version")
            true
        } catch (e: Exception) {
            val currentPath = System.getenv("PATH") ?: "PATH not set"
            logger.warn("""
                git-ai CLI not available: ${e.message}
                Searched locations: ${lastSearchedPaths.joinToString(", ")}
                PATH: $currentPath
            """.trimIndent(), e)
            TelemetryService.getInstance().captureError(e, mapOf(
                "context" to "git_ai_availability_check",
                "searched_paths" to lastSearchedPaths.joinToString(","),
                "current_path" to currentPath
            ))
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

        val gitAiPath = resolvedGitAiPath
        if (gitAiPath == null) {
            logger.warn("Skipping checkpoint - git-ai path not resolved")
            return false
        }

        return try {
            val jsonInput = input.toJson()
            val inputType = when (input) {
                is AgentV1Input.Human -> "human"
                is AgentV1Input.AiAgent -> "ai_agent (${input.agentName})"
            }

            logger.info("Creating checkpoint (agent-v1): $inputType")
            logger.info("Checkpoint input: $jsonInput")

            val command = listOf(gitAiPath, "checkpoint", "agent-v1", "--hook-input", "stdin")
            val process = ProcessBuilder(command)
                .directory(File(workingDirectory))
                .redirectErrorStream(false)
                .start()

            // Write JSON to stdin
            process.outputStream.bufferedWriter().use { writer ->
                writer.write(jsonInput)
            }

            val completed = process.waitFor(30, TimeUnit.SECONDS)
            if (!completed) {
                process.destroyForcibly()
                logger.warn("git-ai checkpoint timed out")
                TelemetryService.getInstance().reportCheckpointTimeout()
                return false
            }

            val output = process.inputStream.bufferedReader().readText().trim()
            val errorOutput = process.errorStream.bufferedReader().readText().trim()
            val exitCode = process.exitValue()

            if (exitCode != 0) {
                val combinedOutput = if (errorOutput.isNotEmpty()) "$output\n$errorOutput" else output
                logger.warn("""
                    git-ai checkpoint failed
                    Command: ${command.joinToString(" ")}
                    Exit code: $exitCode
                    Stdout: $output
                    Stderr: $errorOutput
                """.trimIndent())
                TelemetryService.getInstance().reportCheckpointFailure(exitCode, combinedOutput)
                return false
            }

            logger.info("Checkpoint created successfully ($inputType)")
            if (output.isNotEmpty()) {
                logger.info("git-ai output: $output")
            }
            true
        } catch (e: Exception) {
            logger.warn("Failed to create checkpoint: ${e.message}", e)
            TelemetryService.getInstance().captureError(e, mapOf("context" to "checkpoint_creation"))
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
            resolvedGitAiPath = null
            lastSearchedPaths = emptyList()
        }
    }

    companion object {
        fun getInstance(): GitAiService = service()
    }
}
