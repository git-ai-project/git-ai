//! Tool classification: maps (agent, tool_name) → FileEdit | Bash | Skip.
//!
//! This is the single source of truth for which tools produce file changes
//! vs shell commands vs should be ignored.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolClass {
    FileEdit,
    Bash,
    Skip,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agent {
    Claude,
    Gemini,
    ContinueCli,
    Droid,
    Amp,
    OpenCode,
    Firebender,
    Codex,
    Pi,
    Windsurf,
    Cursor,
    GithubCopilot,
    AiTab,
}

pub fn classify_tool(agent: Agent, tool_name: &str) -> ToolClass {
    match agent {
        Agent::Claude => match tool_name {
            "Write" | "Edit" | "MultiEdit" => ToolClass::FileEdit,
            "Bash" => ToolClass::Bash,
            _ => ToolClass::Skip,
        },
        Agent::Gemini => match tool_name {
            "write_file" | "replace" => ToolClass::FileEdit,
            "shell" => ToolClass::Bash,
            _ => ToolClass::Skip,
        },
        Agent::ContinueCli => match tool_name {
            "edit" => ToolClass::FileEdit,
            "terminal" | "local_shell_call" => ToolClass::Bash,
            _ => ToolClass::Skip,
        },
        Agent::Droid => match tool_name {
            "ApplyPatch" | "Edit" | "Write" | "Create" => ToolClass::FileEdit,
            "Bash" => ToolClass::Bash,
            _ => ToolClass::Skip,
        },
        Agent::Amp => match tool_name {
            "Write" | "Edit" => ToolClass::FileEdit,
            "Bash" => ToolClass::Bash,
            _ => ToolClass::Skip,
        },
        Agent::OpenCode => match tool_name {
            "edit" | "write" => ToolClass::FileEdit,
            "bash" | "shell" => ToolClass::Bash,
            _ => ToolClass::Skip,
        },
        Agent::Firebender => match tool_name {
            "Write" | "Edit" | "Delete" | "RenameSymbol" | "DeleteSymbol" => ToolClass::FileEdit,
            "Bash" => ToolClass::Bash,
            _ => ToolClass::Skip,
        },
        Agent::Codex => match tool_name {
            "apply_patch" => ToolClass::FileEdit,
            "Bash" | "exec_command" | "shell" | "shell_command" => ToolClass::Bash,
            _ => ToolClass::Skip,
        },
        Agent::Pi => match tool_name {
            "edit" | "write" | "replace" | "rename" => ToolClass::FileEdit,
            "bash" => ToolClass::Bash,
            _ => ToolClass::Skip,
        },
        Agent::Windsurf => match tool_name {
            "code_action" => ToolClass::FileEdit,
            "run_command" => ToolClass::Bash,
            _ => ToolClass::Skip,
        },
        Agent::Cursor => match tool_name {
            "Write" | "Delete" | "StrReplace" => ToolClass::FileEdit,
            "Shell" => ToolClass::Bash,
            _ => ToolClass::Skip,
        },
        Agent::GithubCopilot => match tool_name {
            "copilot_replaceString" | "create_file" | "apply_patch" | "editFiles"
            | "insert_edit" | "replace_edit" | "delete_edit"
            | "replace_string_in_file" | "replaceStringInFile"
            | "edit" | "create" => ToolClass::FileEdit,
            "runInTerminal" | "run_in_terminal" => ToolClass::Bash,
            // GitHub Copilot's before_edit/after_edit events may not include a tool_name;
            // default to FileEdit when tool_name is empty (the event type itself implies file edit).
            "" => ToolClass::FileEdit,
            _ => ToolClass::Skip,
        },
        Agent::AiTab => match tool_name {
            // ai_tab is always a file edit (it's inline completion)
            _ => ToolClass::FileEdit,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_tools() {
        assert_eq!(classify_tool(Agent::Cursor, "Write"), ToolClass::FileEdit);
        assert_eq!(classify_tool(Agent::Cursor, "Delete"), ToolClass::FileEdit);
        assert_eq!(classify_tool(Agent::Cursor, "StrReplace"), ToolClass::FileEdit);
        assert_eq!(classify_tool(Agent::Cursor, "Shell"), ToolClass::Bash);
        assert_eq!(classify_tool(Agent::Cursor, "Read"), ToolClass::Skip);
        assert_eq!(classify_tool(Agent::Cursor, "unknown"), ToolClass::Skip);
    }

    #[test]
    fn claude_tools() {
        assert_eq!(classify_tool(Agent::Claude, "Write"), ToolClass::FileEdit);
        assert_eq!(classify_tool(Agent::Claude, "Edit"), ToolClass::FileEdit);
        assert_eq!(classify_tool(Agent::Claude, "MultiEdit"), ToolClass::FileEdit);
        assert_eq!(classify_tool(Agent::Claude, "Bash"), ToolClass::Bash);
        assert_eq!(classify_tool(Agent::Claude, "Read"), ToolClass::Skip);
    }

    #[test]
    fn gemini_tools() {
        assert_eq!(classify_tool(Agent::Gemini, "write_file"), ToolClass::FileEdit);
        assert_eq!(classify_tool(Agent::Gemini, "replace"), ToolClass::FileEdit);
        assert_eq!(classify_tool(Agent::Gemini, "shell"), ToolClass::Bash);
        assert_eq!(classify_tool(Agent::Gemini, "read_file"), ToolClass::Skip);
    }

    #[test]
    fn codex_tools() {
        assert_eq!(classify_tool(Agent::Codex, "apply_patch"), ToolClass::FileEdit);
        assert_eq!(classify_tool(Agent::Codex, "Bash"), ToolClass::Bash);
        assert_eq!(classify_tool(Agent::Codex, "exec_command"), ToolClass::Bash);
        assert_eq!(classify_tool(Agent::Codex, "shell"), ToolClass::Bash);
        assert_eq!(classify_tool(Agent::Codex, "shell_command"), ToolClass::Bash);
    }

    #[test]
    fn copilot_tools() {
        assert_eq!(classify_tool(Agent::GithubCopilot, "copilot_replaceString"), ToolClass::FileEdit);
        assert_eq!(classify_tool(Agent::GithubCopilot, "create_file"), ToolClass::FileEdit);
        assert_eq!(classify_tool(Agent::GithubCopilot, "runInTerminal"), ToolClass::Bash);
        assert_eq!(classify_tool(Agent::GithubCopilot, "unknown"), ToolClass::Skip);
    }

    #[test]
    fn windsurf_tools() {
        assert_eq!(classify_tool(Agent::Windsurf, "code_action"), ToolClass::FileEdit);
        assert_eq!(classify_tool(Agent::Windsurf, "run_command"), ToolClass::Bash);
        assert_eq!(classify_tool(Agent::Windsurf, "search"), ToolClass::Skip);
    }

    #[test]
    fn amp_tools() {
        assert_eq!(classify_tool(Agent::Amp, "Write"), ToolClass::FileEdit);
        assert_eq!(classify_tool(Agent::Amp, "Edit"), ToolClass::FileEdit);
        assert_eq!(classify_tool(Agent::Amp, "Bash"), ToolClass::Bash);
        assert_eq!(classify_tool(Agent::Amp, "Read"), ToolClass::Skip);
    }
}
