using System.Collections.Generic;
using System.Text.Json;
using GitAiVS.Models;
using Microsoft.VisualStudio.TestTools.UnitTesting;

namespace GitAiVS.Tests
{
    [TestClass]
    public class CheckpointInputTests
    {
        [TestMethod]
        public void HumanInputSerializesWithCorrectType()
        {
            var input = new HumanInput
            {
                RepoWorkingDir = "/repo",
                WillEditFilepaths = new List<string> { "src/main.cs" },
                DirtyFiles = new Dictionary<string, string> { { "src/main.cs", "content" } },
            };

            var json = input.ToJson();
            var doc = JsonDocument.Parse(json);

            Assert.AreEqual("human", doc.RootElement.GetProperty("type").GetString());
            Assert.AreEqual("/repo", doc.RootElement.GetProperty("repo_working_dir").GetString());
            Assert.IsTrue(doc.RootElement.TryGetProperty("will_edit_filepaths", out _));
            Assert.IsTrue(doc.RootElement.TryGetProperty("dirty_files", out _));
        }

        [TestMethod]
        public void AiAgentInputSerializesWithCorrectType()
        {
            var input = new AiAgentInput
            {
                RepoWorkingDir = "/repo",
                EditedFilepaths = new List<string> { "src/main.cs" },
                AgentName = "github-copilot-visualstudio",
                Model = "unknown",
                ConversationId = "12345",
            };

            var json = input.ToJson();
            var doc = JsonDocument.Parse(json);

            Assert.AreEqual("ai_agent", doc.RootElement.GetProperty("type").GetString());
            Assert.AreEqual("github-copilot-visualstudio", doc.RootElement.GetProperty("agent_name").GetString());
            Assert.AreEqual("unknown", doc.RootElement.GetProperty("model").GetString());
            Assert.AreEqual("12345", doc.RootElement.GetProperty("conversation_id").GetString());
        }

        [TestMethod]
        public void HumanInputOmitsNullFields()
        {
            var input = new HumanInput
            {
                RepoWorkingDir = "/repo",
            };

            var json = input.ToJson();
            var doc = JsonDocument.Parse(json);

            Assert.IsFalse(doc.RootElement.TryGetProperty("will_edit_filepaths", out _));
            Assert.IsFalse(doc.RootElement.TryGetProperty("dirty_files", out _));
        }

        [TestMethod]
        public void KnownHumanInputSerializesAllFields()
        {
            var input = new KnownHumanInput
            {
                Editor = "visualstudio",
                EditorVersion = "17.10.0",
                ExtensionVersion = "0.1.0",
                Cwd = "/repo",
                EditedFilepaths = new List<string> { "README.md" },
                DirtyFiles = new Dictionary<string, string> { { "README.md", "# Hello" } },
            };

            var json = input.ToJson();
            var doc = JsonDocument.Parse(json);

            Assert.AreEqual("visualstudio", doc.RootElement.GetProperty("editor").GetString());
            Assert.AreEqual("17.10.0", doc.RootElement.GetProperty("editor_version").GetString());
            Assert.AreEqual("0.1.0", doc.RootElement.GetProperty("extension_version").GetString());
            Assert.AreEqual("/repo", doc.RootElement.GetProperty("cwd").GetString());
        }

        [TestMethod]
        public void KnownHumanInputOmitsCwdWhenNull()
        {
            var input = new KnownHumanInput
            {
                Cwd = null,
            };

            var json = input.ToJson();
            var doc = JsonDocument.Parse(json);

            Assert.IsFalse(doc.RootElement.TryGetProperty("cwd", out _));
        }

        [TestMethod]
        public void JsonOptionsSerializesUnannotatedPropertiesAsSnakeCase()
        {
            var json = JsonSerializer.Serialize(
                new NamingPolicyProbe
                {
                    RepoWorkingDir = "/repo",
                    URLValue = "https://example.test",
                },
                JsonOptions.Default);
            var doc = JsonDocument.Parse(json);

            Assert.AreEqual("/repo", doc.RootElement.GetProperty("repo_working_dir").GetString());
            Assert.AreEqual("https://example.test", doc.RootElement.GetProperty("url_value").GetString());
            Assert.IsFalse(doc.RootElement.TryGetProperty("repoWorkingDir", out _));
        }

        private sealed class NamingPolicyProbe
        {
            public string RepoWorkingDir { get; set; } = "";
            public string URLValue { get; set; } = "";
        }
    }
}
