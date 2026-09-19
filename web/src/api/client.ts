import type {
  Agent,
  ChatMessage,
  MemoryEntry,
  ScheduledTask,
  SystemHealth,
  Thread,
  Workspace,
} from './types'

/**
 * Mock API client.
 *
 * Everything here mirrors the shape of the future RustFox Axum endpoints.
 * To go live, swap `mockFetch` for real `fetch` calls — the signatures and
 * return types stay identical, so no component code has to change.
 */

const LATENCY_MS = 220

function delay<T>(value: T): Promise<T> {
  return new Promise((resolve) => setTimeout(() => resolve(value), LATENCY_MS))
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const WORKSPACES: Workspace[] = [
  {
    id: 'soul',
    name: 'Soul (Fox)',
    description: 'The main Telegram assistant — soul files, memory, skills.',
    agentCount: 4,
    updatedAt: '2026-09-18T08:10:00Z',
  },
  {
    id: 'research',
    name: 'Research Lab',
    description: 'arXiv digests, paper deep-dives, experiment pipelines.',
    agentCount: 3,
    updatedAt: '2026-09-18T06:00:00Z',
  },
  {
    id: 'threads',
    name: 'Threads Publisher',
    description: 'Draft → verify → publish pipeline for @chinkan.ai.',
    agentCount: 2,
    updatedAt: '2026-09-17T12:00:00Z',
  },
]

const AGENTS: Agent[] = [
  { id: 'a1', workspaceId: 'soul', name: 'fox', model: 'anthropic/claude-opus-4.8', status: 'running', lastActive: '2026-09-18T08:07:00Z' },
  { id: 'a2', workspaceId: 'soul', name: 'verifier', model: 'openai/gpt-5.5', status: 'idle', lastActive: '2026-09-18T07:42:00Z' },
  { id: 'a3', workspaceId: 'soul', name: 'vault-workflow', model: 'google/gemini-3.1-pro', status: 'idle', lastActive: '2026-09-18T07:30:00Z' },
  { id: 'a4', workspaceId: 'soul', name: 'thread-writer-hk', model: 'anthropic/claude-sonnet-4.8', status: 'error', lastActive: '2026-09-17T23:12:00Z' },
  { id: 'a5', workspaceId: 'research', name: 'exp-explorer', model: 'anthropic/claude-opus-4.8', status: 'idle', lastActive: '2026-09-18T05:55:00Z' },
  { id: 'a6', workspaceId: 'research', name: 'exp-planner', model: 'openai/gpt-5.5', status: 'idle', lastActive: '2026-09-18T05:50:00Z' },
  { id: 'a7', workspaceId: 'research', name: 'exp-executor', model: 'local/qwen3.6-35b-a3b', status: 'running', lastActive: '2026-09-18T08:00:00Z' },
  { id: 'a8', workspaceId: 'threads', name: 'news-fetcher', model: 'openai/gpt-5.5', status: 'idle', lastActive: '2026-09-18T04:00:00Z' },
  { id: 'a9', workspaceId: 'threads', name: 'threads-chain-poster', model: 'anthropic/claude-sonnet-4.8', status: 'idle', lastActive: '2026-09-18T04:12:00Z' },
]

const THREADS: Thread[] = [
  { id: 't1', workspaceId: 'soul', title: 'Weekly vault review', messageCount: 24, updatedAt: '2026-09-18T08:01:00Z' },
  { id: 't2', workspaceId: 'soul', title: 'TanStack Router for the portal', messageCount: 12, updatedAt: '2026-09-18T07:55:00Z' },
  { id: 't3', workspaceId: 'soul', title: 'HSBC statement parse', messageCount: 8, updatedAt: '2026-09-16T22:30:00Z' },
  { id: 't4', workspaceId: 'research', title: 'Memory upgrade phase 1', messageCount: 41, updatedAt: '2026-09-17T18:00:00Z' },
]

const MESSAGES: Record<string, ChatMessage[]> = {
  t1: [
    { id: 'm1', role: 'user', content: '幫我 review 下今個禮拜 vault 有咩新 idea', createdAt: '2026-09-18T07:50:00Z' },
    { id: 'm2', role: 'assistant', content: '今個禮拜有 3 個新 idea 入咗 incubating：TanStack Router portal、multi-bot factory、同 Cantonese TTS。要唔要我逐個總結？', createdAt: '2026-09-18T07:51:00Z' },
  ],
  t2: [
    { id: 'm3', role: 'user', content: 'TanStack Router 適合我哋個 web portal 嗎？', createdAt: '2026-09-18T07:54:00Z' },
    { id: 'm4', role: 'assistant', content: '適合。佢係純前端 router，配 Axum API 正好分工，而且 search params 100% type-safe。', createdAt: '2026-09-18T07:55:00Z' },
  ],
}

const MEMORY: MemoryEntry[] = [
  { id: 'k1', kind: 'fact', text: 'Kan prefers HKT for all match reporting', score: 0.98, createdAt: '2026-06-19T00:00:00Z' },
  { id: 'k2', kind: 'fact', text: 'RustFox is a self-hosted Telegram AI assistant written in Rust', score: 0.95, createdAt: '2026-06-01T00:00:00Z' },
  { id: 'k3', kind: 'knowledge', text: 'HKO weather API endpoints: rhrread, fnd, warnsum', score: 0.91, createdAt: '2026-06-23T00:00:00Z' },
  { id: 'k4', kind: 'knowledge', text: 'notesmd-cli does not exist — use direct file writes for daily notes', score: 0.88, createdAt: '2026-06-23T00:00:00Z' },
  { id: 'k5', kind: 'conversation', text: 'Threads API create_reply has a propagation race condition', score: 0.84, createdAt: '2026-06-19T00:00:00Z' },
  { id: 'k6', kind: 'fact', text: 'RustFox VPS has RTX 5090 Laptop 24GB VRAM', score: 0.99, createdAt: '2026-06-29T00:00:00Z' },
]

const TASKS: ScheduledTask[] = [
  { id: 'c1', name: '香港天氣報告', cron: '0 30 7 * * *', enabled: true, nextRun: '2026-09-19T07:30:00+08:00' },
  { id: 'c2', name: 'arXiv AI 每日簡報', cron: '0 0 10 * * *', enabled: true, nextRun: '2026-09-19T10:00:00+08:00' },
  { id: 'c3', name: 'AI 新聞 → Threads', cron: '0 0 12 * * *', enabled: true, nextRun: '2026-09-19T12:00:00+08:00' },
  { id: 'c4', name: 'Vault GitHub sync', cron: '0 0 22 * * *', enabled: false, nextRun: '—' },
]

const HEALTH: SystemHealth = {
  cpuPercent: 23,
  memUsedGb: 6.4,
  memTotalGb: 15,
  diskUsedPercent: 22,
  uptimeHours: 341,
}

// ---------------------------------------------------------------------------
// Endpoints
// ---------------------------------------------------------------------------

export const api = {
  listWorkspaces: () => delay(WORKSPACES),

  getWorkspace: (id: string) =>
    delay(WORKSPACES.find((w) => w.id === id) ?? null),

  listAgents: (workspaceId?: string) =>
    delay(workspaceId ? AGENTS.filter((a) => a.workspaceId === workspaceId) : AGENTS),

  listThreads: (workspaceId?: string) =>
    delay(workspaceId ? THREADS.filter((t) => t.workspaceId === workspaceId) : THREADS),

  getThread: (id: string) => delay(THREADS.find((t) => t.id === id) ?? null),

  getMessages: (threadId: string) => delay(MESSAGES[threadId] ?? []),

  searchMemory: (query: string, kind?: MemoryEntry['kind']) => {
    const q = query.trim().toLowerCase()
    const results = MEMORY.filter((m) => {
      const matchesText = q.length === 0 || m.text.toLowerCase().includes(q)
      const matchesKind = !kind || m.kind === kind
      return matchesText && matchesKind
    })
    return delay(results)
  },

  listTasks: () => delay(TASKS),

  getHealth: () => delay(HEALTH),
}

export type Api = typeof api
