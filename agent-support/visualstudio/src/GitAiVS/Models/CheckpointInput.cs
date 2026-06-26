using System.Collections.Generic;
using System.Text;
using System.Text.Json;
using System.Text.Json.Serialization;

namespace GitAiVS.Models
{
    /// <summary>
    /// Base class for agent-v1 checkpoint inputs sent to:
    ///   git-ai checkpoint agent-v1 --hook-input stdin
    /// 
    /// The JSON is tagged with "type" to match the Rust AgentV1Payload enum
    /// (deserialized via #[serde(tag = "type", rename_all = "snake_case")]).
    /// </summary>
    public abstract class AgentV1Input
    {
        [JsonPropertyName("type")]
        public abstract string Type { get; }

        [JsonPropertyName("repo_working_dir")]
        public string RepoWorkingDir { get; set; } = "";

        public abstract string ToJson();
    }

    /// <summary>
    /// Human (before_edit) checkpoint — captures file state before an AI edit begins.
    /// </summary>
    public sealed class HumanInput : AgentV1Input
    {
        [JsonPropertyName("type")]
        public override string Type => "human";

        [JsonPropertyName("will_edit_filepaths")]
        [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
        public List<string>? WillEditFilepaths { get; set; }

        [JsonPropertyName("dirty_files")]
        [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
        public Dictionary<string, string>? DirtyFiles { get; set; }

        public override string ToJson() => JsonSerializer.Serialize(this, JsonOptions.Default);
    }

    /// <summary>
    /// AI agent (after_edit) checkpoint — records changes made by an AI agent.
    /// </summary>
    public sealed class AiAgentInput : AgentV1Input
    {
        [JsonPropertyName("type")]
        public override string Type => "ai_agent";

        [JsonPropertyName("edited_filepaths")]
        [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
        public List<string>? EditedFilepaths { get; set; }

        [JsonPropertyName("agent_name")]
        public string AgentName { get; set; } = "";

        [JsonPropertyName("model")]
        public string Model { get; set; } = "unknown";

        [JsonPropertyName("conversation_id")]
        public string ConversationId { get; set; } = "";

        [JsonPropertyName("dirty_files")]
        [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
        public Dictionary<string, string>? DirtyFiles { get; set; }

        public override string ToJson() => JsonSerializer.Serialize(this, JsonOptions.Default);
    }

    /// <summary>
    /// Known-human checkpoint input sent to:
    ///   git-ai checkpoint known_human --hook-input stdin
    /// </summary>
    public sealed class KnownHumanInput
    {
        [JsonPropertyName("editor")]
        public string Editor { get; set; } = "visualstudio";

        [JsonPropertyName("editor_version")]
        public string EditorVersion { get; set; } = "";

        [JsonPropertyName("extension_version")]
        public string ExtensionVersion { get; set; } = "";

        [JsonPropertyName("cwd")]
        [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
        public string? Cwd { get; set; }

        [JsonPropertyName("edited_filepaths")]
        public List<string> EditedFilepaths { get; set; } = new();

        [JsonPropertyName("dirty_files")]
        public Dictionary<string, string> DirtyFiles { get; set; } = new();

        public string ToJson() => JsonSerializer.Serialize(this, JsonOptions.Default);
    }

    internal static class JsonOptions
    {
        public static readonly JsonSerializerOptions Default = new()
        {
            DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull,
            PropertyNamingPolicy = SnakeCaseJsonNamingPolicy.Instance,
            WriteIndented = false,
        };
    }

    internal sealed class SnakeCaseJsonNamingPolicy : JsonNamingPolicy
    {
        public static readonly SnakeCaseJsonNamingPolicy Instance = new();

        public override string ConvertName(string name)
        {
            if (string.IsNullOrEmpty(name))
                return name;

            var builder = new StringBuilder(name.Length + 8);

            for (var i = 0; i < name.Length; i++)
            {
                var value = name[i];
                if (char.IsUpper(value))
                {
                    if (ShouldInsertUnderscore(name, i))
                        builder.Append('_');

                    builder.Append(char.ToLowerInvariant(value));
                }
                else
                {
                    builder.Append(value);
                }
            }

            return builder.ToString();
        }

        private static bool ShouldInsertUnderscore(string name, int index)
        {
            if (index == 0 || name[index - 1] == '_')
                return false;

            if (char.IsLower(name[index - 1]) || char.IsDigit(name[index - 1]))
                return true;

            return index + 1 < name.Length && char.IsLower(name[index + 1]);
        }
    }
}
