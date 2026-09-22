import { SseHttpError, streamSse } from './sse'
import type {
  Agent,
  ChatHistory,
  LoginResponse,
  MeResponse,
  MemoryEntry,
  MemoryKind,
  ReloadResult,
  ScheduledTask,
  Settings,
  SettingsPatch,
  SettingsPatchResult,
  Skill,
  SoulFile,
  SoulName,
  Stats,
  StreamFrame,
  SystemHealth,
  TaskRun,
  TaskToggleResult,
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
  enableTask: (id: string) =>
    request<TaskToggleResult>(`/tasks/${encodeURIComponent(id)}/enable`, { method: 'POST' }),
  disableTask: (id: string) =>
    request<TaskToggleResult>(`/tasks/${encodeURIComponent(id)}/disable`, { method: 'POST' }),

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
}

export type Api = typeof api
