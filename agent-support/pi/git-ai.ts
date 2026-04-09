import type { ExtensionAPI } from "@mariozechner/pi-coding-agent";
import { spawn } from "node:child_process";
import { readFile } from "node:fs/promises";
import { homedir } from "node:os";
import { resolve, isAbsolute, join } from "node:path";

const GIT_AI_BIN = '__GIT_AI_BINARY_PATH__';
// Optional user-owned override file. Managed installer never writes it.
const OVERRIDE_CONFIG_PATH = join(homedir(), '.pi', 'agent', 'git-ai.override.json');

type CanonicalToolName = 'edit' | 'write' | 'replace' | 'rename' | 'bash';
type HookEventName = 'before_edit' | 'after_edit' | 'before_command' | 'after_command';

type ToolOverridePolicy = {
  kind: 'mutating';
  canonical: CanonicalToolName;
  filepath_fields: string[];
} | {
  kind: 'ignore';
};

type OverrideConfig = {
  version: 1;
  tools: Record<string, ToolOverridePolicy>;
};

const DEFAULT_TOOL_POLICIES: Record<string, ToolOverridePolicy> = {
  edit: {
    kind: 'mutating',
    canonical: 'edit',
    filepath_fields: ['path'],
  },
  write: {
    kind: 'mutating',
    canonical: 'write',
    filepath_fields: ['path'],
  },
};

function isCanonicalToolName(value: unknown): value is CanonicalToolName {
  return value === 'edit' || value === 'write' || value === 'replace' || value === 'rename' || value === 'bash';
}

function isStringArray(value: unknown): value is string[] {
  return Array.isArray(value) && value.every((entry) => typeof entry === 'string' && entry.trim().length > 0);
}

function normalizeToolPolicy(value: unknown): ToolOverridePolicy | undefined {
  if (typeof value !== 'object' || value === null) return undefined;
  const raw = value as Record<string, unknown>;

  if (raw.kind === 'ignore') {
    return { kind: 'ignore' };
  }

  if (
    raw.kind === 'mutating'
    && isCanonicalToolName(raw.canonical)
    && isStringArray(raw.filepath_fields)
  ) {
    return {
      kind: 'mutating',
      canonical: raw.canonical,
      filepath_fields: raw.filepath_fields,
    };
  }

  return undefined;
}

async function loadOverrideConfig(): Promise<OverrideConfig | undefined> {
  try {
    const raw = await readFile(OVERRIDE_CONFIG_PATH, 'utf8');
    const parsed = JSON.parse(raw);
    if (parsed?.version !== 1 || typeof parsed?.tools !== 'object' || parsed.tools === null) {
      return undefined;
    }

    const tools: Record<string, ToolOverridePolicy> = {};
    for (const [toolName, policy] of Object.entries(parsed.tools as Record<string, unknown>)) {
      if (typeof toolName !== 'string' || toolName.trim().length === 0) continue;
      const normalized = normalizeToolPolicy(policy);
      if (!normalized) continue;
      tools[toolName] = normalized;
    }

    return { version: 1, tools };
  } catch {
    return undefined;
  }
}

async function buildToolConfig() {
  const override = await loadOverrideConfig();
  const mergedPolicies = new Map<string, ToolOverridePolicy>(
    Object.entries({
      ...DEFAULT_TOOL_POLICIES,
      ...(override?.tools ?? {}),
    }),
  );

  return {
    toolPolicies: mergedPolicies,
  };
}

type MutatingCall = {
  toolNameRaw: string;
  toolName: CanonicalToolName;
  filepaths: string[];
  toolInput: unknown;
};

type CheckpointPayload = {
  hook_event_name: HookEventName;
  session_id: string;
  session_path: string;
  cwd: string;
  model: string;
  tool_name: CanonicalToolName;
  tool_name_raw: string;
  tool_use_id?: string;
  dirty_files?: Record<string, string>;
  will_edit_filepaths?: string[];
  edited_filepaths?: string[];
  tool_input?: unknown;
  tool_result?: unknown;
};

async function readSessionLines(sessionPath: string): Promise<string[]> {
  const content = await readFile(sessionPath, 'utf8');
  return content.split(/\r?\n/).filter((line) => line.trim().length > 0);
}

async function readSessionId(sessionPath: string): Promise<string | undefined> {
  const lines = await readSessionLines(sessionPath);
  const header = JSON.parse(lines[0] ?? '{}');
  if (header?.type === 'session' && typeof header.id === 'string' && header.id.trim().length > 0) {
    return header.id;
  }
  return undefined;
}

async function readLatestAssistantModel(sessionPath: string): Promise<string> {
  const lines = await readSessionLines(sessionPath);
  let latestModel = '';

  for (const line of lines) {
    const entry = JSON.parse(line);
    if (entry?.type !== 'message' || entry?.message?.role !== 'assistant') continue;
    const model = typeof entry.message.model === 'string' ? entry.message.model : '';
    if (model) latestModel = model;
  }

  return latestModel;
}

function resolveAbsolutePath(cwd: string, pathValue: string): string {
  return isAbsolute(pathValue) ? pathValue : resolve(cwd, pathValue);
}

function normalizeFilepaths(cwd: string, rawPaths: unknown[]): string[] {
  const unique = new Set<string>();
  for (const rawPath of rawPaths) {
    if (typeof rawPath !== 'string' || rawPath.trim().length === 0) continue;
    unique.add(resolveAbsolutePath(cwd, rawPath));
  }
  return [...unique];
}

function extractDirectFilepaths(
  input: any,
  filepathFields: string[],
): string[] {
  const rawPaths: string[] = [];

  for (const field of filepathFields) {
    const value = input?.[field];
    if (typeof value === 'string') {
      rawPaths.push(value);
      continue;
    }
    if (Array.isArray(value)) {
      rawPaths.push(...value.filter((entry): entry is string => typeof entry === 'string'));
    }
  }

  return rawPaths;
}

function toMutatingCall(
  cwd: string,
  toolNameRaw: string,
  input: unknown,
  policy: ToolOverridePolicy,
): MutatingCall | undefined {
  if (policy.kind !== 'mutating') return undefined;

  const filepaths = normalizeFilepaths(
    cwd,
    extractDirectFilepaths(input as any, policy.filepath_fields),
  );
  if (filepaths.length === 0) return undefined;

  return { toolNameRaw, toolName: policy.canonical, filepaths, toolInput: input };
}

function extractMutatingCalls(
  cwd: string,
  toolNameRaw: string,
  input: unknown,
  toolConfig: {
    toolPolicies: Map<string, ToolOverridePolicy>;
  },
): MutatingCall[] {
  const policy = toolConfig.toolPolicies.get(toolNameRaw);
  if (!policy || policy.kind !== 'mutating') {
    return [];
  }

  const direct = toMutatingCall(
    cwd,
    toolNameRaw,
    input,
    policy,
  );
  return direct ? [direct] : [];
}

async function readDirtyFiles(filepaths: string[]): Promise<Record<string, string>> {
  const dirtyFiles: Record<string, string> = {};

  await Promise.all(
    filepaths.map(async (filepath) => {
      try {
        dirtyFiles[filepath] = await readFile(filepath, 'utf8');
      } catch {
        dirtyFiles[filepath] = '';
      }
    }),
  );

  return dirtyFiles;
}

async function runCheckpoint(payload: CheckpointPayload): Promise<void> {
  await new Promise<void>((resolvePromise) => {
    const child = spawn(GIT_AI_BIN, ['checkpoint', 'pi', '--hook-input', 'stdin'], {
      stdio: ['pipe', 'ignore', 'ignore'],
    });

    child.on('error', () => resolvePromise());
    child.on('close', () => resolvePromise());
    child.stdin.end(JSON.stringify(payload));
  });
}

export default function (pi: ExtensionAPI) {
  const pendingCalls = new Map<string, MutatingCall[]>();
  const pendingBashCalls = new Set<string>();
  const toolConfigPromise = buildToolConfig();

  pi.on('tool_call', async (event, ctx) => {
    const sessionPath = ctx.sessionManager.getSessionFile();
    if (!sessionPath) return;

    // Bash tool path — snapshot system handles filesystem diffing
    if (event.toolName === 'bash') {
      const sessionId = await readSessionId(sessionPath).catch(() => undefined);
      if (!sessionId) return;
      const model = await readLatestAssistantModel(sessionPath).catch(() => '');
      pendingBashCalls.add(event.toolCallId);
      await runCheckpoint({
        hook_event_name: 'before_command',
        session_id: sessionId,
        session_path: sessionPath,
        cwd: ctx.cwd,
        model,
        tool_name: 'bash',
        tool_name_raw: event.toolName,
        tool_use_id: event.toolCallId,
      });
      return;
    }

    // File-edit tool path (existing logic)
    const mutatingCalls = extractMutatingCalls(
      ctx.cwd,
      event.toolName,
      event.input,
      await toolConfigPromise,
    );
    if (mutatingCalls.length === 0) return;

    const sessionId = await readSessionId(sessionPath).catch(() => undefined);
    if (!sessionId) return;

    const model = await readLatestAssistantModel(sessionPath).catch(() => '');
    pendingCalls.set(event.toolCallId, mutatingCalls);

    for (const call of mutatingCalls) {
      await runCheckpoint({
        hook_event_name: 'before_edit',
        session_id: sessionId,
        session_path: sessionPath,
        cwd: ctx.cwd,
        model,
        tool_name: call.toolName,
        tool_name_raw: call.toolNameRaw,
        will_edit_filepaths: call.filepaths,
        dirty_files: await readDirtyFiles(call.filepaths),
        tool_input: call.toolInput,
      });
    }
  });

  pi.on('tool_result', async (event, ctx) => {
    // Bash tool result path
    if (pendingBashCalls.has(event.toolCallId)) {
      pendingBashCalls.delete(event.toolCallId);
      if (event.isError) return;
      const sessionPath = ctx.sessionManager.getSessionFile();
      if (!sessionPath) return;
      const sessionId = await readSessionId(sessionPath).catch(() => undefined);
      if (!sessionId) return;
      const model = await readLatestAssistantModel(sessionPath).catch(() => '');
      await runCheckpoint({
        hook_event_name: 'after_command',
        session_id: sessionId,
        session_path: sessionPath,
        cwd: ctx.cwd,
        model,
        tool_name: 'bash',
        tool_name_raw: 'bash',
        tool_use_id: event.toolCallId,
      });
      return;
    }

    // File-edit tool result path (existing logic)
    if (event.isError) {
      pendingCalls.delete(event.toolCallId);
      return;
    }

    const sessionPath = ctx.sessionManager.getSessionFile();
    if (!sessionPath) {
      pendingCalls.delete(event.toolCallId);
      return;
    }

    const mutatingCalls = pendingCalls.get(event.toolCallId) ?? [];
    pendingCalls.delete(event.toolCallId);
    if (mutatingCalls.length === 0) return;

    const sessionId = await readSessionId(sessionPath).catch(() => undefined);
    if (!sessionId) return;

    const model = await readLatestAssistantModel(sessionPath).catch(() => '');

    for (const call of mutatingCalls) {
      await runCheckpoint({
        hook_event_name: 'after_edit',
        session_id: sessionId,
        session_path: sessionPath,
        cwd: ctx.cwd,
        model,
        tool_name: call.toolName,
        tool_name_raw: call.toolNameRaw,
        edited_filepaths: call.filepaths,
        dirty_files: await readDirtyFiles(call.filepaths),
        tool_input: call.toolInput,
        tool_result: {
          content: event.content,
          details: event.details,
          isError: event.isError,
        },
      });
    }
  });
}
