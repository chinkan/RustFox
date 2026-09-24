import { SseHttpError, streamSse } from './sse'
import type {
  Agent,
  AgentDetail,
  ChatHistory,
  CreateEntryBody,
  CreateEntryResult,
  EntryDetail,
  FileWriteResult,
  InstallRequest,
  InstallResult,
  InstalledLedgerView,
  InstallDryRunResult,
  LoginResponse,
  MeResponse,
  MemoryEntry,
  MemoryKind,
  QuarantineResult,
  ReloadResult,
  ScheduledTask,
  Settings,
  SettingsPatch,
  SettingsPatchResult,
  Skill,
  SkillEntry,
  SoulFile,
  SoulName,
  Stats,
  StreamFrame,
  SystemHealth,
  TaskCreateBody,
  TaskCreateResult,
  TaskDeleteResult,
  TaskEnableResult,
  TaskRun,
  TaskToggleResult,
  TaskUpdateBody,
  TaskUpdateResult,
  Thread,
} from './types'

/**
 * Real API client — talks to the RustFox Axum portal (`/api/*`).
 *
 * Auth is a HttpOnly session cookie from `POST /api/auth/login`
 * (ADR 0008A), so every request is `credentials: 'same-origin'` and no
 * token ever touches JS storage. Errors arrive as the standard envelope
 * `{"error":{"code","message"}}` and are thrown as `ApiError`.
 */

const BASE = '/api'

export class ApiError extends Error {
  constructor(
    message: string,
    public readonly status: number,
    public readonly code: string,
  ) {
    super(message)
    this.name = 'ApiError'
  }
}

async function request<T>(path: string, init: RequestInit = {}): Promise<T> {
  let res: Response
  try {
    res = await fetch(`${BASE}${path}`, {
      credentials: 'same-origin',
      ...init,
      headers: {
        ...(init.body ? { 'Content-Type': 'application/json' } : {}),
        ...(init.headers ?? {}),
      },
    })
  } catch (e) {
    // Network-level failure (server down / restart in flight).
    throw new ApiError(e instanceof Error ? e.message : 'network error', 0, 'network_error')
  }

  if (res.status === 204) return undefined as T

  let payload: unknown = null
  const text = await res.text()
  if (text) {
    try {
      payload = JSON.parse(text)
    } catch {
      throw new ApiError(`expected JSON from ${path}, got ${res.status}`, res.status, 'bad_json')
    }
  }

  if (!res.ok) {
    const env = (payload as { error?: { code?: string; message?: string } })?.error
    throw new ApiError(env?.message ?? `HTTP ${res.status}`, res.status, env?.code ?? '')
  }
  return payload as T
}

const get = <T>(path: string) => request<T>(path)

export const api = {
  // ── auth ────────────────────────────────────────────────────────────────
  login: (token: string) =>
    request<LoginResponse>('/auth/login', { method: 'POST', body: JSON.stringify({ token }) }),
  logout: (everywhere = false) =>
    request<void>('/auth/logout', { method: 'POST', body: JSON.stringify({ everywhere }) }),
  me: () => get<MeResponse>('/auth/me'),

  // ── chat ────────────────────────────────────────────────────────────────
  getHistory: () => get<ChatHistory>('/chat/history'),
  listThreads: () => get<Thread[]>('/chat/threads'),
  cancelChat: () => request<{ cancelled: boolean }>('/chat/cancel', { method: 'POST' }),
  /**
   * POST /api/chat and stream SSE frames. The caller owns the AbortSignal;
   * aborting closes the stream (the server run continues — use cancelChat()
   * to actually stop generation).
   */
  chatStream: (
    text: string,
    onFrame: (f: StreamFrame) => void,
    signal?: AbortSignal,
  ): Promise<void> =>
    streamSse(`${BASE}/chat`, { text }, onFrame, signal).catch((e: unknown) => {
      // One error type at the boundary: 409/chat_in_progress must arrive as
      // ApiError like every other failure, or the UI can't branch on code.
      if (e instanceof SseHttpError) throw new ApiError(e.message, e.status, e.code)
      throw e
    }),

  // ── dashboard ───────────────────────────────────────────────────────────
  getHealth: () => get<SystemHealth>('/health'),
  getStats: () => get<Stats>('/stats'),

  // ── agents & skills ─────────────────────────────────────────────────────
  listAgents: () => get<Agent[]>('/agents'),
  listSkills: () => get<Skill[]>('/agents/skills'),
  reloadAgents: () => request<ReloadResult>('/agents/reload', { method: 'POST' }),

  // ── memory ──────────────────────────────────────────────────────────────
  searchMemory: (q: string, kind?: MemoryKind, limit = 50) => {
    const params = new URLSearchParams({ q, limit: String(limit) })
    if (kind) params.set('kind', kind)
    return get<MemoryEntry[]>(`/memory/search?${params}`)
  },

  // ── tasks ───────────────────────────────────────────────────────────────
  listTasks: () => get<ScheduledTask[]>('/tasks'),
  getTaskRuns: (id: string) => get<TaskRun[]>(`/tasks/${encodeURIComponent(id)}/runs`),
  // enableTask / disableTask live in the task-CRUD section below (T3: real re-arm).

  // ── settings & soul ─────────────────────────────────────────────────────
  getSettings: () => get<Settings>('/settings'),
  patchSettings: (patch: SettingsPatch) =>
    request<SettingsPatchResult>('/settings', { method: 'PATCH', body: JSON.stringify(patch) }),
  getSoul: (name: SoulName) => get<SoulFile>(`/soul?name=${encodeURIComponent(name)}`),
  putSoul: (name: SoulName, content: string) =>
    request<{ ok: boolean; name: string; bytes: number }>('/soul', {
      method: 'PUT',
      body: JSON.stringify({ name, content }),
    }),

  // ── control plane: skills (ADR 0011) ────────────────────────────────────
  /** Rich list with provenance/drift. kind = 'skills' | 'agents' | 'all'. */
  listEntries: (kind?: 'skills' | 'agents') =>
    get<SkillEntry[]>(`/skills${kind ? `?kind=${kind}` : ''}`),
  getSkill: (name: string) => get<EntryDetail>(`/skills/${encodeURIComponent(name)}`),
  getSkillFile: (name: string, path: string) =>
    get<{ path: string; content: string; size: number }>(
      `/skills/${encodeURIComponent(name)}/file?path=${encodeURIComponent(path)}`,
    ),
  /**
   * Update-only file write. Pass `baseHash` (from detail `hash` / primary
   * `fileHash` / `files[].hash`) for the optimistic lock (ADR 0011a R4) —
   * omit to force-write (CLI parity). 409 changed_since_read means
   * somebody else (OpenCode, another tab) touched the file: re-read first.
   */
  putSkillFile: (
    name: string,
    body: { path: string; content: string; baseHash?: string },
  ) =>
    request<FileWriteResult>(`/skills/${encodeURIComponent(name)}/file`, {
      method: 'PUT',
      body: JSON.stringify(body),
    }),
  createSkill: (body: CreateEntryBody) =>
    request<CreateEntryResult>('/skills', { method: 'POST', body: JSON.stringify(body) }),
  deleteSkill: (name: string) =>
    request<QuarantineResult>(`/skills/${encodeURIComponent(name)}`, { method: 'DELETE' }),

  // ── control plane: agents ───────────────────────────────────────────────
  getAgentDetail: (name: string) => get<AgentDetail>(`/agents/${encodeURIComponent(name)}`),
  getAgentFile: (name: string, path: string) =>
    get<{ path: string; content: string; size: number }>(
      `/agents/${encodeURIComponent(name)}/file?path=${encodeURIComponent(path)}`,
    ),
  /** `allowMissing` = escape hatch for unknown tools (400 unknown_tools). */
  putAgentFile: (
    name: string,
    body: { path: string; content: string; baseHash?: string },
    allowMissing = false,
  ) =>
    request<FileWriteResult>(
      `/agents/${encodeURIComponent(name)}/file${allowMissing ? '?allowMissing=1' : ''}`,
      { method: 'PUT', body: JSON.stringify(body) },
    ),
  createAgent: (body: CreateEntryBody) =>
    request<CreateEntryResult>(`/agents`, { method: 'POST', body: JSON.stringify(body) }),
  deleteAgent: (name: string) =>
    request<QuarantineResult>(`/agents/${encodeURIComponent(name)}`, { method: 'DELETE' }),

  // ── GitHub skill installer ──────────────────────────────────────────────
  /**
   * POST /api/skills/install. Two-step (ADR 0011a R2): dryRun:true first →
   * verdict; then dryRun:false with acknowledgedWarnings = exact warningKey()
   * strings the owner ticked. Unacked warnings come back as 409
   * warnings_unacknowledged.
   */
  installSkill: (req: InstallRequest) =>
    request<InstallResult | InstallDryRunResult>('/skills/install', {
      method: 'POST',
      body: JSON.stringify(req),
    }),
  listInstalled: () => get<InstalledLedgerView>('/skills/installed'),

  // ── task CRUD (T3) — real re-arm, soft delete ───────────────────────────
  createTask: (body: TaskCreateBody) =>
    request<TaskCreateResult>('/tasks', { method: 'POST', body: JSON.stringify(body) }),
  updateTask: (id: string, body: TaskUpdateBody) =>
    request<TaskUpdateResult>(`/tasks/${encodeURIComponent(id)}`, {
      method: 'PUT',
      body: JSON.stringify(body),
    }),
  deleteTask: (id: string) =>
    request<TaskDeleteResult>(`/tasks/${encodeURIComponent(id)}`, { method: 'DELETE' }),
  /** Real re-arm: the returned schedulerJobId/nextRun are live, no restart needed. */
  enableTask: (id: string) =>
    request<TaskEnableResult>(`/tasks/${encodeURIComponent(id)}/enable`, { method: 'POST' }),
  disableTask: (id: string) =>
    request<TaskToggleResult>(`/tasks/${encodeURIComponent(id)}/disable`, { method: 'POST' }),
}

export type Api = typeof api
