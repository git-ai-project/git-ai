// @i-know-the-amp-plugin-api-is-wip-and-very-experimental-right-now
// Required by Amp: this exact marker must remain the first line in the plugin file.
import type { PluginAPI, ToolCallEvent } from '@ampcode/plugin'
import { spawn } from 'child_process'
import { randomUUID } from 'crypto'
import { mkdir, mkdtemp, rename, writeFile } from 'fs/promises'
import { tmpdir } from 'os'
import { dirname, isAbsolute, resolve } from 'path'
import { fileURLToPath } from 'url'

// Absolute path to git-ai binary, replaced at install time by `git-ai install-hooks`
const GIT_AI_BIN = '__GIT_AI_BINARY_PATH__'
const AMP_CHECKPOINT_TOOLS = new Set([
	'Write',
	'Edit',
	'create_file',
	'edit_file',
	'apply_patch',
	'undo_edit',
	'Bash',
	'shell_command',
])

type PendingToolCall = {
	cwd: string
	tool: string
	toolInput: Record<string, unknown>
	editedFilepaths: string[]
	threadId: string
	transcriptPath: string | null
}

function uniqueNonEmptyPaths(paths: string[]): string[] {
	const deduped = new Set<string>()
	for (const path of paths) {
		const trimmed = path.trim()
		if (trimmed) {
			deduped.add(trimmed)
		}
	}
	return Array.from(deduped)
}

function pathFromFileUri(uri: string): string | null {
	try {
		const parsed = new URL(uri)
		if (parsed.protocol !== 'file:') {
			return null
		}
		return fileURLToPath(parsed)
	} catch {
		return null
	}
}

function filesFromInput(input: Record<string, unknown>): string[] {
	const paths: string[] = []
	for (const key of ['path', 'filePath', 'file_path']) {
		const value = input[key]
		if (typeof value === 'string') {
			paths.push(value)
		}
	}

	for (const key of ['paths', 'filePaths', 'filepaths']) {
		const value = input[key]
		if (!Array.isArray(value)) {
			continue
		}
		for (const item of value) {
			if (typeof item === 'string') {
				paths.push(item)
			}
		}
	}

	return uniqueNonEmptyPaths(paths)
}

function filesFromToolCall(amp: PluginAPI, event: ToolCallEvent): string[] {
	const fromHelper = amp.helpers.filesModifiedByToolCall(event)
	const helperPaths =
		fromHelper
			?.map((uri) => pathFromFileUri(uri.toString()))
			.filter((value): value is string => value !== null) ?? []

	if (helperPaths.length > 0) {
		return uniqueNonEmptyPaths(helperPaths)
	}

	return filesFromInput(event.input)
}

export default function ampGitAiPlugin(amp: PluginAPI) {
	const pendingCalls = new Map<string, PendingToolCall>()
	const transcriptExports = new Map<string, Promise<string | null>>()
	const lastTranscriptPaths = new Map<string, string>()
	let exportDirPromise: Promise<string> | null = null
	let gitAiInstalledPromise: Promise<boolean> | null = null

	const getExportDir = (): Promise<string> => {
		if (exportDirPromise) {
			return exportDirPromise
		}
		const creating = mkdtemp(resolve(tmpdir(), 'git-ai-amp-'))
		exportDirPromise = creating
		void creating.catch(() => {
			if (exportDirPromise === creating) {
				exportDirPromise = null
			}
		})
		return creating
	}

	const runProcess = (
		command: string,
		args: string[],
		options?: { stdin?: string; timeoutMs?: number },
	): Promise<{ exitCode: number; stdout: string; stderr: string }> => {
		return new Promise((resolve, reject) => {
			const child = spawn(command, args, {
				stdio: ['pipe', 'pipe', 'pipe'],
			})
			const timeout =
				typeof options?.timeoutMs === 'number'
					? setTimeout(() => child.kill(), options.timeoutMs)
					: null

			let stdout = ''
			let stderr = ''

			child.stdout.on('data', (chunk: unknown) => {
				stdout += chunk.toString()
			})
			child.stderr.on('data', (chunk: unknown) => {
				stderr += chunk.toString()
			})
			child.on('error', reject)
			child.on('close', (code) => {
				if (timeout) {
					clearTimeout(timeout)
				}
				resolve({
					exitCode: code ?? -1,
					stdout,
					stderr,
				})
			})

			if (typeof options?.stdin === 'string') {
				child.stdin.write(options.stdin)
			}
			child.stdin.end()
		})
	}

	const exportOrbTranscript = (
		threadId: string,
		ctx: { logger: PluginAPI['logger'] },
	): Promise<string | null> => {
		const existing = transcriptExports.get(threadId)
		if (existing) {
			return existing
		}
		const previousTranscriptPath = lastTranscriptPaths.get(threadId) ?? null

		const exported = (async () => {
			try {
				const result = await runProcess('amp', ['threads', 'export', threadId], {
					timeoutMs: 5_000,
				})
				if (result.exitCode !== 0) {
					const stderr = result.stderr.trim()
					ctx.logger.log(
						`[git-ai] unable to export Amp thread ${threadId} (exit=${result.exitCode})${
							stderr ? `: ${stderr}` : ''
						}`,
					)
					return previousTranscriptPath
				}

				const transcript = JSON.parse(result.stdout) as { messages?: unknown }
				if (!Array.isArray(transcript.messages)) {
					ctx.logger.log(`[git-ai] Amp thread ${threadId} export has no messages`)
					return null
				}
				const hasModel = transcript.messages.some(
					(message) =>
						typeof message === 'object' &&
						message !== null &&
						'usage' in message &&
						typeof message.usage === 'object' &&
						message.usage !== null &&
						'model' in message.usage &&
						typeof message.usage.model === 'string',
				)
				if (!hasModel) {
					return previousTranscriptPath
				}

				const exportDir = await getExportDir()
				await mkdir(exportDir, { recursive: true, mode: 0o700 })
				const safeThreadId = threadId.replace(/[^A-Za-z0-9_-]/g, '_')
				const transcriptPath = resolve(exportDir, `${safeThreadId}.json`)
				const stagedPath = resolve(exportDir, `${safeThreadId}.${randomUUID()}.tmp`)
				await writeFile(stagedPath, result.stdout, { flag: 'wx', mode: 0o600 })
				await rename(stagedPath, transcriptPath)
				lastTranscriptPaths.set(threadId, transcriptPath)
				return transcriptPath
			} catch (error) {
				ctx.logger.log(`[git-ai] unable to export Amp thread ${threadId}`, String(error))
				return previousTranscriptPath
			}
		})()

		transcriptExports.set(threadId, exported)
		void exported.finally(() => {
			if (transcriptExports.get(threadId) === exported) {
				transcriptExports.delete(threadId)
			}
		})
		return exported
	}

	const detectGitRoot = async (args: string[]): Promise<string | null> => {
		try {
			const result = await runProcess('git', args)
			if (result.exitCode !== 0) {
				return null
			}

			const root = result.stdout.trim()
			return root || null
		} catch {
			return null
		}
	}

	const ensureGitAiInstalled = async () => {
		if (!gitAiInstalledPromise) {
			gitAiInstalledPromise = (async () => {
				try {
					const result = await runProcess(GIT_AI_BIN, ['--version'])
					return result.exitCode === 0
				} catch {
					return false
				}
			})()
		}
		return gitAiInstalledPromise
	}

	const resolveRepoRoot = async (filepaths: string[]): Promise<string> => {
		const fromCurrentDir = await detectGitRoot(['rev-parse', '--show-toplevel'])
		if (fromCurrentDir) {
			return fromCurrentDir
		}

		const candidates = new Set<string>()
		const baseDirs = [process.cwd()]
		if (typeof process.env.PWD === 'string' && process.env.PWD.trim()) {
			baseDirs.push(process.env.PWD)
		}
		for (const dir of baseDirs) {
			candidates.add(dir)
		}
		for (const path of filepaths) {
			if (isAbsolute(path)) {
				candidates.add(dirname(path))
				continue
			}
			for (const baseDir of baseDirs) {
				candidates.add(dirname(resolve(baseDir, path)))
			}
		}

		for (const candidate of candidates) {
			const root = await detectGitRoot(['-C', candidate, 'rev-parse', '--show-toplevel'])
			if (root) {
				return root
			}
		}

		return process.cwd()
	}

	const runCheckpoint = async (ctx: { logger: PluginAPI['logger'] }, payload: Record<string, unknown>) => {
		try {
			const hookInput = JSON.stringify(payload)
			const result = await runProcess(
				GIT_AI_BIN,
				['checkpoint', 'amp', '--hook-input', 'stdin'],
				{ stdin: hookInput },
			)

			const stderr = result.stderr.trim()
			const shouldLogStderr =
				result.exitCode !== 0 ||
				/Failed to find any git repositories|preset error|Error: --hook-input/i.test(stderr)
			if (shouldLogStderr) {
				ctx.logger.log(
					`[git-ai] checkpoint amp failed (exit=${result.exitCode})${
						stderr ? `: ${stderr}` : ''
					}`,
				)
			}
		} catch (error) {
			ctx.logger.log('[git-ai] checkpoint amp failed', String(error))
		}
	}

	amp.on('tool.call', async (event, ctx) => {
		if (!AMP_CHECKPOINT_TOOLS.has(event.tool)) {
			return { action: 'allow' as const }
		}

		const gitAiInstalled = await ensureGitAiInstalled()
		if (!gitAiInstalled) {
			return { action: 'allow' as const }
		}

		const editedFilepaths = filesFromToolCall(amp, event)
		const repoCwd = await resolveRepoRoot(editedFilepaths)
		const threadId = event.thread.id
		const transcriptPath = await exportOrbTranscript(threadId, { logger: ctx.logger })

		const pending: PendingToolCall = {
			cwd: repoCwd,
			tool: event.tool,
			toolInput: event.input,
			editedFilepaths,
			threadId,
			transcriptPath,
		}
		pendingCalls.set(event.toolUseID, pending)

		await runCheckpoint(
			{ logger: ctx.logger },
			{
				hook_event_name: 'PreToolUse',
				tool_use_id: event.toolUseID,
				tool_name: pending.tool,
				tool_input: pending.toolInput,
				cwd: pending.cwd,
				thread_id: pending.threadId,
				...(pending.transcriptPath ? { transcript_path: pending.transcriptPath } : {}),
				...(pending.editedFilepaths.length > 0
					? { edited_filepaths: pending.editedFilepaths }
					: {}),
			},
		)

		return { action: 'allow' as const }
	})

	amp.on('tool.result', async (event, ctx) => {
		const pending = pendingCalls.get(event.toolUseID)
		if (!pending) {
			return
		}
		pendingCalls.delete(event.toolUseID)

		if (event.status !== 'done') {
			return
		}
		const transcriptPath =
			(await exportOrbTranscript(pending.threadId, { logger: ctx.logger })) ??
			pending.transcriptPath

		await runCheckpoint(
			{ logger: ctx.logger },
			{
				hook_event_name: 'PostToolUse',
				tool_use_id: event.toolUseID,
				tool_name: pending.tool,
				tool_input: pending.toolInput,
				cwd: pending.cwd,
				thread_id: pending.threadId,
				...(transcriptPath ? { transcript_path: transcriptPath } : {}),
				...(pending.editedFilepaths.length > 0
					? { edited_filepaths: pending.editedFilepaths }
					: {}),
			},
		)
	})
}
