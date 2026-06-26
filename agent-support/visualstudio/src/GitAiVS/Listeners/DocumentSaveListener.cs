using System;
using System.Collections.Concurrent;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Threading;
using System.Threading.Tasks;
using GitAiVS.Services;

namespace GitAiVS.Listeners
{
    /// <summary>
    /// Listens for document save events and fires known_human checkpoints.
    /// Debounces per workspace root (500ms) to batch multiple saves.
    /// Filters out Visual Studio internal paths (.vs/).
    ///
    /// Modeled after IntelliJ's DocumentSaveListener.kt.
    /// </summary>
    public sealed class DocumentSaveListener : IDisposable
    {
        private const int DebounceMs = 500;

        private readonly CheckpointService _checkpointSvc;
        private readonly string _editorVersion;
        private readonly string _extensionVersion;

        private readonly ConcurrentDictionary<string, CancellationTokenSource> _pendingCheckpoints = new();
        private readonly ConcurrentDictionary<string, ConcurrentBag<string>> _pendingPaths = new();

        public DocumentSaveListener(CheckpointService checkpointSvc, string editorVersion, string extensionVersion)
        {
            _checkpointSvc = checkpointSvc;
            _editorVersion = editorVersion;
            _extensionVersion = extensionVersion;
        }

        /// <summary>
        /// Called when a document is saved via IVsRunningDocTableEvents.OnAfterSave.
        /// </summary>
        public void OnDocumentSaved(string filePath)
        {
            if (IsInternalPath(filePath))
            {
                Trace.WriteLine($"[git-ai] [SAVE] Skipping internal path: {filePath}");
                return;
            }

            var workspaceRoot = GitRepoResolver.FindRepoRoot(filePath);
            if (workspaceRoot == null) return;

            Trace.WriteLine($"[git-ai] [SAVE] Document saved: {Path.GetFileName(filePath)}");

            var bag = _pendingPaths.GetOrAdd(workspaceRoot, _ => new ConcurrentBag<string>());
            bag.Add(filePath);

            ScheduleCheckpoint(workspaceRoot);
        }

        private void ScheduleCheckpoint(string workspaceRoot)
        {
            if (_pendingCheckpoints.TryRemove(workspaceRoot, out var existingCts))
                existingCts.Cancel();

            var cts = new CancellationTokenSource();
            _pendingCheckpoints[workspaceRoot] = cts;

            _ = Task.Delay(DebounceMs, cts.Token).ContinueWith(t =>
            {
                if (t.IsCanceled) return;

                _pendingCheckpoints.TryRemove(workspaceRoot, out CancellationTokenSource _);
                ExecuteCheckpoint(workspaceRoot);
            }, TaskScheduler.Default);
        }

        private void ExecuteCheckpoint(string workspaceRoot)
        {
            if (!_pendingPaths.TryRemove(workspaceRoot, out var bag))
                return;

            var paths = bag.ToArray().Distinct().ToList();
            if (paths.Count == 0) return;

            var dirtyFiles = new Dictionary<string, string>();
            var editedPaths = new List<string>();

            foreach (var absolutePath in paths)
            {
                try
                {
                    if (!File.Exists(absolutePath)) continue;
                    var content = File.ReadAllText(absolutePath);
                    dirtyFiles[absolutePath] = content;
                    editedPaths.Add(absolutePath);
                }
                catch
                {
                    Trace.WriteLine($"[git-ai] [SAVE] Could not read file: {absolutePath}");
                }
            }

            if (editedPaths.Count == 0) return;

            Trace.WriteLine($"[git-ai] [SAVE] Firing known_human checkpoint for {editedPaths.Count} file(s)");

#pragma warning disable VSTHRD110
            _ = _checkpointSvc.SendKnownHumanAsync(
                workspaceRoot,
                _editorVersion,
                _extensionVersion,
                editedPaths,
                dirtyFiles);
#pragma warning restore VSTHRD110
        }

        internal static bool IsInternalPath(string path)
        {
            return path.Contains($"{Path.DirectorySeparatorChar}.vs{Path.DirectorySeparatorChar}")
                || path.Contains("/.vs/");
        }

        public void Dispose()
        {
            foreach (var cts in _pendingCheckpoints.Values)
            {
                cts.Cancel();
                cts.Dispose();
            }
            _pendingCheckpoints.Clear();
        }
    }
}
