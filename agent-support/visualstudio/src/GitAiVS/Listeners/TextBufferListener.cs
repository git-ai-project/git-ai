using System;
using System.Collections.Concurrent;
using System.Collections.Generic;
using System.ComponentModel.Composition;
using System.Diagnostics;
using System.IO;
using System.Threading;
using System.Threading.Tasks;
using GitAiVS.Detection;
using GitAiVS.Services;
using Microsoft.VisualStudio.Editor;
using Microsoft.VisualStudio.Text;
using Microsoft.VisualStudio.Text.Editor;
using Microsoft.VisualStudio.TextManager.Interop;
using Microsoft.VisualStudio.Utilities;

namespace GitAiVS.Listeners
{
    /// <summary>
    /// Attaches to every opened text editor in Visual Studio and listens for
    /// ITextBuffer.Changed events. Uses stack trace analysis to detect AI edits
    /// and dispatches before_edit (human) and after_edit (ai_agent) checkpoints.
    ///
    /// Modeled after IntelliJ's DocumentChangeListener.kt.
    /// </summary>
    [Export(typeof(IVsTextViewCreationListener))]
    [ContentType("text")]
    [TextViewRole(PredefinedTextViewRoles.Editable)]
    public sealed class TextBufferListener : IVsTextViewCreationListener
    {
        [Import]
        internal IVsEditorAdaptersFactoryService? AdapterService { get; set; }

        private readonly ConcurrentDictionary<string, CancellationTokenSource> _pendingCheckpoints = new();
        private readonly ConcurrentDictionary<string, long> _beforeEditTriggered = new();

        private static readonly object SubscriptionStateKey = typeof(TextBufferListener);

        private const int DebounceMs = 300;
        private const long BeforeEditExpiryMs = 5000;

        private CheckpointService? CheckpointSvc => CheckpointService.Current;

        public void VsTextViewCreated(IVsTextView textViewAdapter)
        {
            if (AdapterService == null) return;

            var textView = AdapterService.GetWpfTextView(textViewAdapter);
            if (textView == null) return;

            var buffer = textView.TextBuffer;
            var filePath = GetFilePath(buffer);
            Trace.WriteLine($"[git-ai] TextBufferListener attached to: {filePath ?? "(unknown)"}");

            AddBufferSubscription(buffer);

            textView.Closed += (_, __) =>
            {
                RemoveBufferSubscription(buffer);
            };
        }

        private void AddBufferSubscription(ITextBuffer buffer)
        {
            lock (buffer.Properties)
            {
                if (!buffer.Properties.TryGetProperty(SubscriptionStateKey, out BufferSubscriptionState state))
                {
                    state = new BufferSubscriptionState();
                    buffer.Properties.AddProperty(SubscriptionStateKey, state);
                    buffer.Changed += OnBufferChanged;
                }

                state.ViewCount++;
            }
        }

        private void RemoveBufferSubscription(ITextBuffer buffer)
        {
            lock (buffer.Properties)
            {
                if (!buffer.Properties.TryGetProperty(SubscriptionStateKey, out BufferSubscriptionState state))
                    return;

                state.ViewCount--;
                if (state.ViewCount > 0)
                    return;

                buffer.Changed -= OnBufferChanged;
                buffer.Properties.RemoveProperty(SubscriptionStateKey);
            }
        }

        private void OnBufferChanged(object sender, TextContentChangedEventArgs e)
        {
            var buffer = sender as ITextBuffer;
            if (buffer == null) return;

            var filePath = GetFilePath(buffer);
            if (filePath == null) return;

            var stackTrace = new StackTrace();
            var analysis = CopilotEditDetector.Analyze(stackTrace);

            LogBufferChange(filePath, analysis);

            if (CheckpointSvc == null) return;

            if (analysis.Confidence != Confidence.High || analysis.AgentName == null)
                return;

            var workspaceRoot = GitRepoResolver.FindRepoRoot(filePath);
            if (workspaceRoot == null) return;

            var now = DateTimeOffset.UtcNow.ToUnixTimeMilliseconds();

            var contentAfterEdit = e.After.GetText();

            SendBeforeEditIfNeeded(filePath, workspaceRoot, analysis.AgentName, e, now);
            ScheduleAfterEditCheckpoint(filePath, workspaceRoot, analysis.AgentName, contentAfterEdit);
        }

        private void SendBeforeEditIfNeeded(string filePath, string workspaceRoot, string agentName, TextContentChangedEventArgs e, long now)
        {
            if (_beforeEditTriggered.TryGetValue(filePath, out var lastTriggered) && (now - lastTriggered) < BeforeEditExpiryMs)
                return;

            _beforeEditTriggered[filePath] = now;

            var preEditContent = e.Before.GetText();
            var relativePath = GitRepoResolver.ToRelativePath(filePath, workspaceRoot);
            var dirtyFiles = new Dictionary<string, string> { { relativePath, preEditContent } };

            Trace.WriteLine($"[git-ai] Triggering human checkpoint (before edit by {agentName}) on {relativePath}");

#pragma warning disable VSTHRD110
            _ = Task.Run(() => CheckpointSvc!.SendBeforeEditAsync(
                workspaceRoot,
                new[] { relativePath },
                dirtyFiles));
#pragma warning restore VSTHRD110
        }

        private void ScheduleAfterEditCheckpoint(string filePath, string workspaceRoot, string agentName, string capturedContent)
        {
            if (_pendingCheckpoints.TryRemove(filePath, out var existingCts))
                existingCts.Cancel();

            var cts = new CancellationTokenSource();
            _pendingCheckpoints[filePath] = cts;

            _ = Task.Delay(DebounceMs, cts.Token).ContinueWith(t =>
            {
                if (t.IsCanceled) return;

                _pendingCheckpoints.TryRemove(filePath, out CancellationTokenSource _);
                _beforeEditTriggered.TryRemove(filePath, out long _);

                var relativePath = GitRepoResolver.ToRelativePath(filePath, workspaceRoot);
                var dirtyFiles = new Dictionary<string, string> { { relativePath, capturedContent } };

                Trace.WriteLine($"[git-ai] Triggering ai_agent checkpoint for {agentName} on {relativePath}");

#pragma warning disable VSTHRD110
                CheckpointSvc?.SendAfterEditAsync(
                    workspaceRoot,
                    new[] { relativePath },
                    agentName,
                    dirtyFiles);
#pragma warning restore VSTHRD110
            }, TaskScheduler.Default);
        }

        private static string? GetFilePath(ITextBuffer buffer)
        {
            if (buffer.Properties.TryGetProperty(typeof(ITextDocument), out ITextDocument document))
                return document.FilePath;

            return null;
        }

        private static void LogBufferChange(string filePath, AnalysisResult analysis)
        {
            if (analysis.AgentName == null) return;

            Trace.WriteLine($"[git-ai] Buffer change detected on {Path.GetFileName(filePath)}");
            Trace.WriteLine($"[git-ai]   Source: {analysis.AgentName} (confidence: {analysis.Confidence})");
            Trace.WriteLine($"[git-ai]   Relevant frames:\n{CopilotEditDetector.FormatRelevantFrames(analysis.RelevantFrames)}");
        }

        private sealed class BufferSubscriptionState
        {
            public int ViewCount { get; set; }
        }
    }
}
