import { IDEHostKind } from "./utils/host-kind";

export const MIN_GIT_AI_VERSION = "1.0.23";
export const MIN_VSCODE_NATIVE_HOOKS_VERSION = "1.109.3";

// Use GitHub URL to avoid VS Code open URL safety prompt
export const GIT_AI_INSTALL_DOCS_URL = "https://github.com/git-ai-project/git-ai?tab=readme-ov-file#quick-start";

// IDE-specific AI tab completion commands
export const TAB_AI_COMPLETION_COMMANDS: Partial<Record<IDEHostKind, string>> = {
  'cursor': 'editor.action.acceptCursorTabSuggestion', // Cursor AI tab accepted (AI edit shows up between before and after hooks)
  'vscode': 'editor.action.inlineSuggest.commit', // VS Code AI tab accepted (AI edit shows up between before and after hooks)
};

// Notes: For future inline chat detection
// 'inlineChat.acceptChanges'; // VS Code inline AI chat accepted (changes were already applied)
// 'inlineChat.start'; // VS Code inline AI chat opened (user is about to type a prompt)
