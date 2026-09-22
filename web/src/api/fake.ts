import type { Api } from './client'

/**
 * A typed fake `api` object for unit/integration tests — every route the
 * SPA touches, backed by plain fixtures. No network, no MSW needed.
 */

export function fakeApi(over: Partial<Api> = {}): Api {
  const agents = [
    { id: 'main', name: 'rustfox', model: 'anthropic/claude-opus-4.8', status: 'idle' as const, lastActive: null, platform: 'telegram+web' },
    { id: 'agent:verifier', name: 'verifier', model: 'openai/gpt-5.5', status: 'running' as const, lastActive: null, platform: 'subagent' },
    { id: 'agent:exp-executor', name: 'exp-executor', model: 'local/qwen3.6', status: 'running' as const, lastActive: null, platform: 'subagent' },
    { id: 'agent:exp-explorer', name: 'exp-explorer', model: 'anthropic/claude-opus-4.8', status: 'idle' as const, lastActive: null, platform: 'subagent' },
    { id: 'agent:thread-writer-hk', name: 'thread-writer-hk', model: 'anthropic/claude-sonnet-4.8', status: 'error' as const, lastActive: null, platform: 'subagent' },
  ]
  return {
    login: async () => ({ username: 'web', role: 'admin' }),
    logout: async () => undefined,
    me: async () => ({ authenticated: false, username: '', role: '' }),
    getHistory: async () => ({
      conversationId: 'conv-1',
      messages: [
        { id: 'h0', role: 'user' as const, content: 'hello', createdAt: null },
        { id: 'h1', role: 'assistant' as const, content: 'hi kan', createdAt: null },
      ],
    }),
    listThreads: async () => [
      { id: 'conv-1', title: 'hello', messageCount: 2, updatedAt: '2026-09-22T00:00:00Z' },
    ],
    cancelChat: async () => ({ cancelled: true }),
    chatStream: async () => undefined,
    getHealth: async () => ({
      cpuPercent: 10,
      memUsedGb: 4,
      memTotalGb: 16,
      diskUsedPercent: 30,
      uptimeHours: 5,
      bootId: 'boot-aaaa',
    }),
    getStats: async () => ({
      model: 'anthropic/claude-opus-4.8',
      providers: ['openrouter'],
      messageCount: 100,
      conversationCount: 7,
      activeTasks: 3,
      skills: 91,
      portalEnabled: true,
    }),
    listAgents: async () => agents,
    listSkills: async () => [
      { name: 'hko-weather', description: 'HKO weather', kind: 'skill' as const },
      { name: 'verifier', description: 'Zero-trust verifier', kind: 'agent' as const },
    ],
    reloadAgents: async () => ({ skillsLoaded: 91, agentsLoaded: 10 }),
    searchMemory: async (q: string) =>
      [
        { id: 'k1', kind: 'fact' as const, text: 'Kan prefers HKT for all match reporting', score: 0.98, createdAt: null },
        { id: 'k2', kind: 'knowledge' as const, text: 'HKO weather API endpoints', score: 0.91, createdAt: null },
      ].filter((m) => m.text.toLowerCase().includes(q.toLowerCase())),
    listTasks: async () => [
      { id: 'c1', name: '香港天氣報告', cron: '0 30 7 * * *', enabled: true, nextRun: '2026-09-23T07:30:00+08:00' },
      { id: 'c2', name: 'arXiv 簡報', cron: '0 0 10 * * *', enabled: false, nextRun: '—' },
    ],
    getTaskRuns: async () => [
      { id: 1, runAt: '2026-09-22T07:30:00Z', status: 'completed', error: null, response: 'ok' },
    ],
    enableTask: async (id: string) => ({ ok: true, id, enabled: true, restartToSchedule: true }),
    disableTask: async (id: string) => ({ ok: true, id, enabled: false }),
    getSettings: async () => ({
      editable: {
        model: 'anthropic/claude-opus-4.8',
        generalLocation: 'Hong Kong',
        defaultAutonomyMode: 'default',
        portalPort: 8090,
      },
      masked: {
        telegramBotToken: '123…456',
        openrouterApiKey: 'sk-or-…abcd',
        providers: [{ name: 'openrouter', model: 'x', baseUrl: 'https://openrouter.ai/api', apiKeyMasked: 'sk-…1234' }],
        mcpServers: [{ name: 'google-workspace' }],
      },
      restartRequired: ['portalPort'],
    }),
    patchSettings: async (p) => ({
      updated: Object.keys(p),
      restartRequired: 'portalPort' in p ? ['portalPort'] : [],
    }),
    getSoul: async (name: string) => ({ name, content: `# ${name}\nbody`, mtime: '2026-09-22T00:00:00Z' }),
    putSoul: async (name: string, content: string) => ({ ok: true, name, bytes: content.length }),
    ...over,
  }
}
