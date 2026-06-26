using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Threading.Tasks;
using GitAiVS.Models;

namespace GitAiVS.Services
{
    /// <summary>
    /// Spawns the git-ai CLI to create checkpoints.
    /// All methods are fire-and-forget safe -- they never throw.
    ///
    /// Modeled after IntelliJ's GitAiService.kt checkpoint methods.
    /// </summary>
    public sealed class CheckpointService
    {
        private const int CheckpointTimeoutMs = 30_000;

        private readonly BinaryResolver _resolver;
        private readonly string _sessionId;

        /// <summary>
        /// Global singleton so MEF-created components (TextBufferListener) can access it
        /// without manual wiring. Set by GitAiPackage during initialization.
        /// </summary>
        public static CheckpointService? Current { get; set; }

        public CheckpointService(BinaryResolver resolver)
        {
            _resolver = resolver;
            _sessionId = DateTimeOffset.UtcNow.ToUnixTimeMilliseconds().ToString();
        }

        public string SessionId => _sessionId;

        /// <summary>
        /// Send a human (before_edit) checkpoint via agent-v1 preset.
        /// </summary>
        public Task<bool> SendBeforeEditAsync(string repoRoot, string[] willEditPaths, Dictionary<string, string>? dirtyFiles)
        {
            var input = new HumanInput
            {
                RepoWorkingDir = repoRoot,
                WillEditFilepaths = new List<string>(willEditPaths),
                DirtyFiles = dirtyFiles,
            };

            return RunCheckpointAsync("agent-v1", "human", input.ToJson(), repoRoot);
        }

        /// <summary>
        /// Send an AI agent (after_edit) checkpoint via agent-v1 preset.
        /// </summary>
        public Task<bool> SendAfterEditAsync(string repoRoot, string[] editedPaths, string agentName, Dictionary<string, string>? dirtyFiles)
        {
            var input = new AiAgentInput
            {
                RepoWorkingDir = repoRoot,
                EditedFilepaths = new List<string>(editedPaths),
                AgentName = agentName,
                Model = "unknown",
                ConversationId = _sessionId,
                DirtyFiles = dirtyFiles,
            };

            return RunCheckpointAsync("agent-v1", $"ai_agent ({agentName})", input.ToJson(), repoRoot);
        }

        /// <summary>
        /// Send a known_human checkpoint.
        /// </summary>
        public Task<bool> SendKnownHumanAsync(string repoRoot, string editorVersion, string extensionVersion,
            List<string> editedPaths, Dictionary<string, string> dirtyFiles)
        {
            var input = new KnownHumanInput
            {
                Editor = "visualstudio",
                EditorVersion = editorVersion,
                ExtensionVersion = extensionVersion,
                Cwd = repoRoot,
                EditedFilepaths = editedPaths,
                DirtyFiles = dirtyFiles,
            };

            return RunCheckpointAsync("known_human", "known_human", input.ToJson(), repoRoot);
        }

        private async Task<bool> RunCheckpointAsync(string preset, string inputType, string stdinJson, string cwd)
        {
            var binaryPath = await _resolver.ResolveAsync();
            if (binaryPath == null)
            {
                Trace.WriteLine("[git-ai] Skipping checkpoint -- git-ai not available");
                return false;
            }

            try
            {
                var args = $"checkpoint {preset} --hook-input stdin";

                Trace.WriteLine($"[git-ai] Creating checkpoint ({preset}): {inputType}");

                var psi = new ProcessStartInfo
                {
                    FileName = binaryPath,
                    Arguments = args,
                    WorkingDirectory = cwd,
                    UseShellExecute = false,
                    RedirectStandardInput = true,
                    RedirectStandardOutput = true,
                    RedirectStandardError = true,
                    CreateNoWindow = true,
                };

                using var proc = Process.Start(psi);
                if (proc == null)
                {
                    Trace.WriteLine("[git-ai] Failed to start git-ai process");
                    return false;
                }

                await proc.StandardInput.WriteAsync(stdinJson);
                proc.StandardInput.Close();

                var stdoutTask = proc.StandardOutput.ReadToEndAsync();
                var stderrTask = proc.StandardError.ReadToEndAsync();

                var completed = proc.WaitForExit(CheckpointTimeoutMs);
                if (!completed)
                {
                    proc.Kill();
                    Trace.WriteLine($"[git-ai] Checkpoint timed out after {CheckpointTimeoutMs}ms");
                    return false;
                }

                var stdout = (await stdoutTask).Trim();
                var stderr = (await stderrTask).Trim();

                if (proc.ExitCode != 0)
                {
                    Trace.WriteLine($"[git-ai] Checkpoint failed");
                    Trace.WriteLine($"[git-ai]   Command: {binaryPath} {args}");
                    Trace.WriteLine($"[git-ai]   Exit code: {proc.ExitCode}");
                    Trace.WriteLine($"[git-ai]   Stdout: {stdout}");
                    Trace.WriteLine($"[git-ai]   Stderr: {stderr}");
                    return false;
                }

                Trace.WriteLine($"[git-ai] Checkpoint created successfully ({inputType})");
                if (stdout.Length > 0)
                    Trace.WriteLine($"[git-ai]   Output: {stdout}");

                return true;
            }
            catch (Exception ex)
            {
                Trace.WriteLine($"[git-ai] Checkpoint error: {ex.Message}");
                return false;
            }
        }
    }
}
