using System.Collections.Generic;
using System.Diagnostics;
using System.Linq;

namespace GitAiVS.Detection
{
    public enum Confidence
    {
        None,
        Medium,
        High,
    }

    public sealed class AnalysisResult
    {
        public string? AgentName { get; }
        public Confidence Confidence { get; }
        public IReadOnlyList<StackFrame> RelevantFrames { get; }

        public AnalysisResult(string? agentName, Confidence confidence, IReadOnlyList<StackFrame> relevantFrames)
        {
            AgentName = agentName;
            Confidence = confidence;
            RelevantFrames = relevantFrames;
        }
    }

    /// <summary>
    /// Analyzes stack traces to detect which AI agent triggered a document change.
    /// Modeled after IntelliJ's StackTraceAnalyzer.kt.
    /// 
    /// When ITextBuffer.Changed fires, the calling thread's stack contains frames
    /// from whatever code triggered the change. If Copilot triggered it, frames
    /// from Copilot's assemblies will be present.
    /// </summary>
    public static class CopilotEditDetector
    {
        private sealed class AgentPattern
        {
            public string Name { get; }
            public string[] NamespacePrefixes { get; }
            public string[] ClassKeywords { get; }

            public AgentPattern(string name, string[] namespacePrefixes, string[] classKeywords)
            {
                Name = name;
                NamespacePrefixes = namespacePrefixes;
                ClassKeywords = classKeywords;
            }
        }

        private static readonly AgentPattern[] KnownAgents = new[]
        {
            new AgentPattern(
                name: "github-copilot-visualstudio",
                namespacePrefixes: new[]
                {
                    "GitHub.Copilot",
                    "Microsoft.VisualStudio.Copilot",
                    "Microsoft.VisualStudio.Conversations.UI.Internal.Copilot",
                },
                classKeywords: new[] { "copilot" }
            ),
        };

        /// <summary>
        /// Inspect a stack trace for known AI agent patterns.
        /// Only HIGH-confidence results (full namespace prefix match) should trigger checkpoints.
        /// </summary>
        public static AnalysisResult Analyze(StackTrace stackTrace)
        {
            var relevantFrames = new List<StackFrame>();
            string? detectedAgent = null;
            var confidence = Confidence.None;

            for (int i = 0; i < stackTrace.FrameCount; i++)
            {
                var frame = stackTrace.GetFrame(i);
                if (frame == null) continue;

                var method = frame.GetMethod();
                if (method?.DeclaringType == null) continue;

                var fullName = method.DeclaringType.FullName ?? "";
                var lowerName = fullName.ToLowerInvariant();

                foreach (var agent in KnownAgents)
                {
                    bool matchesNamespace = agent.NamespacePrefixes.Any(
                        prefix => fullName.StartsWith(prefix, System.StringComparison.OrdinalIgnoreCase));

                    bool matchesClass = agent.ClassKeywords.Any(
                        keyword => lowerName.Contains(keyword));

                    if (matchesNamespace)
                    {
                        if (detectedAgent == null)
                        {
                            detectedAgent = agent.Name;
                            confidence = Confidence.High;
                            relevantFrames.Add(frame);
                        }
                        else if (detectedAgent == agent.Name)
                        {
                            if (confidence == Confidence.Medium)
                                confidence = Confidence.High;
                            relevantFrames.Add(frame);
                        }
                    }
                    else if (matchesClass)
                    {
                        if (detectedAgent == null)
                        {
                            detectedAgent = agent.Name;
                            confidence = Confidence.Medium;
                            relevantFrames.Add(frame);
                        }
                        else if (detectedAgent == agent.Name)
                        {
                            relevantFrames.Add(frame);
                        }
                    }
                }
            }

            return new AnalysisResult(detectedAgent, confidence, relevantFrames);
        }

        public static string FormatRelevantFrames(IReadOnlyList<StackFrame> frames)
        {
            if (frames.Count == 0)
                return "  (no relevant frames detected)";

            var lines = new List<string>();
            foreach (var frame in frames)
            {
                var method = frame.GetMethod();
                if (method?.DeclaringType == null) continue;
                lines.Add($"  {method.DeclaringType.FullName}.{method.Name}");
            }

            return string.Join("\n", lines);
        }
    }
}
