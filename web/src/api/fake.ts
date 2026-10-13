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
    getScheduleTimezone: async () => ({
      timezone: 'Asia/Hong_Kong',
      utcOffset: '+08:00',
      source: 'config',
    }),
    listTasks: async () => [
      { id: 'c1', name: '香港天氣報告', cron: '0 30 7 * * *', enabled: true, nextRun: '2026-09-23T07:30:00+08:00', prompt: 'Run the hko-weather skill.', triggerType: 'recurring', triggerValue: '0 30 7 * * *', status: 'active', platform: 'telegram', timezone: 'Asia/Hong_Kong' },
      { id: 'c2', name: 'arXiv 簡報', cron: '0 0 10 * * *', enabled: false, nextRun: '—', prompt: 'Run cantonese-arxiv-daily-briefing.', triggerType: 'recurring', triggerValue: '0 0 10 * * *', status: 'paused', platform: 'telegram', timezone: 'UTC' },
    ],
    getTaskRuns: async () => [
      { id: 1, runAt: '2026-09-22T07:30:00Z', status: 'completed', error: null, response: 'ok' },
    ],
    enableTask: async (id: string) => ({ ok: true, id, enabled: true, schedulerJobId: 'job-' + id, nextRun: '2026-09-26T07:30:00+08:00' }),
    disableTask: async (id: string) => ({ ok: true, id, enabled: false, jobRemoved: true }),
    googleStatus: async () => ({ offered: false }),
    startGoogle: async () => ({ offered: false }),
    setGoogleClientId: async () => ({ offered: true }),
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
      systemPrompt: { source: 'builtin' as const, pointer: null, divergence: false },
      sandbox: {
        mode: 'trusted' as const,
        platform: 'linux',
        windowsNote: null,
        canInstall: false,
        installButton: 'Install for me',
        installingHint: 'Installing the safety tool. Your computer may ask for your password; RustFox never sees it.',
        installCommand: 'sudo apt install bubblewrap',
        switchToSafeLabel: 'Switch to safe mode',
      },
    }),
    patchSettings: async (p) => ({
      updated: Object.keys(p),
      restartRequired: 'portalPort' in p ? ['portalPort'] : [],
    }),
    switchSandboxSafe: async () => ({
      ok: true,
      mode: 'sandbox' as const,
      message: 'Safe mode is on. Commands can now only use your workspace folder.',
    }),
    switchSandboxTrusted: async () => ({
      ok: true,
      mode: 'trusted' as const,
      message: 'Trusted mode is on. Commands are not OS-confined.',
    }),
    installSandboxTool: async () => ({
      ok: false,
      mode: 'trusted' as const,
      message: 'We could not install the safety tool. You can install it yourself by running: sudo apt install bubblewrap',
      includesInstallCommand: true,
    }),
    getSoul: async (name: string) => ({ name, content: `# ${name}\nbody`, mtime: '2026-09-22T00:00:00Z' }),
    putSoul: async (name: string, content: string) => ({ ok: true, name, bytes: content.length }),
    // -- control plane (T5) --
    listEntries: async () => [
      { name: 'hko-weather', kind: 'skill' as const, provenance: 'bundled' as const, modified: false, deletable: false },
      { name: 'my-experiment', kind: 'skill' as const, provenance: 'installed' as const, modified: true, deletable: true },
      { name: 'verifier', kind: 'agent' as const, provenance: 'user' as const, modified: false, deletable: true },
    ],
    getSkill: async (name: string) => entryDetail(name),
    getSkillFile: async (name: string, path: string) => ({ path, content: `# ${name} · ${path}\nbody`, size: 24 }),
    putSkillFile: async (name: string, body: { path: string; content: string }) => ({
      ok: true, name, path: body.path, bytes: body.content.length,
      provenance: 'user' as const, skillsLoaded: 92, warnings: [],
    }),
    createSkill: async (body: { name: string; content: string }) => ({
      ok: true, name: body.name, kind: 'skill' as const, path: 'SKILL.md', bytes: body.content.length,
      provenance: 'user' as const, skillsLoaded: 92, agentsLoaded: 10, warnings: [],
    }),
    deleteSkill: async (name: string) => ({ ok: true, name, kind: 'skill' as const, quarantinedTo: `.trash/${name}-20260926`, skillsLoaded: 91, agentsLoaded: 10 }),
    getAgentDetail: async (name: string) => entryDetail(name),
    getAgentFile: async (name: string, path: string) => ({ path, content: `---\nname: ${name}\n---\nbody`, size: 20 }),
    putAgentFile: async (name: string, body: { path: string; content: string }) => ({
      ok: true, name, path: body.path, bytes: body.content.length,
      provenance: 'user' as const, agentsLoaded: 10, warnings: [],
    }),
    createAgent: async (body: { name: string; content: string }) => ({
      ok: true, name: body.name, kind: 'agent' as const, path: 'AGENT.md', bytes: body.content.length,
      provenance: 'user' as const, skillsLoaded: 91, agentsLoaded: 11, warnings: [],
    }),
    deleteAgent: async (name: string) => ({ ok: true, name, kind: 'agent' as const, quarantinedTo: `.trash/${name}-20260926`, skillsLoaded: 91, agentsLoaded: 10 }),
    installSkill: async (req) => req.dryRun
      ? {
          dryRun: true as const,
          source: { owner: 'foo', repo: 'bar', ref: 'abc123' },
          skills: [{ name: 'cool-skill', description: 'does cool things', files: [{ path: 'SKILL.md', size: 1200 }], sizeBytes: 1200 }],
          verdict: {
            refused: [],
            warnings: [{ skill: 'cool-skill', file: 'scripts/run.sh', rule: 'executable', detail: 'contains a shell script' }],
            notes: [],
          },
        }
      : {
          dryRun: false as const,
          installed: ['cool-skill'],
          skipped: [],
          verdict: { refused: [], warnings: [], notes: [] },
          reload: { skillsLoaded: 92, agentsLoaded: 10 },
        },
    listInstalled: async () => ({
      skills: { 'my-experiment': { source_repo: 'foo/bar', commit_sha: 'abc123def', ref: 'main', installed_at: '2026-09-25T10:00:00Z', content_hash: 'aa' } },
      agents: {},
    }),
    createTask: async (body) => ({ id: 'c9', name: body.name, schedulerJobId: 'job-c9', nextRun: '2026-09-26T12:00:00+08:00' }),
    updateTask: async (id, body) => ({
      id,
      updated: { name: body.name ?? '', prompt: body.prompt ?? '', triggerValue: body.triggerValue ?? '' },
      rearmed: true,
      schedulerJobId: 'job-' + id,
      nextRun: '2026-09-26T12:00:00+08:00',
    }),
    deleteTask: async (id) => ({ ok: true, id, softDeleted: true, historyPreserved: true }),
    getSecretClaim: async (_token: string) => ({
      id: 'pend-1',
      name: 'DEMO_SECRET',
      expiresAt: Math.floor(Date.now() / 1000) + 900,
      status: 'pending',
    }),
    submitSecretClaim: async (_token: string, _value: string) => ({
      ok: true,
      name: 'DEMO_SECRET',
      stored: true,
    }),
    cancelSecretClaim: async () => ({ cancelled: true }),
    listPendingSecrets: async () => [],
    createPendingSecret: async (name: string) => ({
      id: 'pend-1',
      name,
      expiresAt: Math.floor(Date.now() / 1000) + 900,
      claimUrl: `http://127.0.0.1:8090/secrets/claim/fake-token`,
      claimToken: 'fake-token',
    }),
    ...over,
  }
}

function entryDetail(name: string) {
  return {
    name,
    kind: name === 'verifier' ? ('agent' as const) : ('skill' as const),
    provenance: 'user' as const,
    modified: false,
    deletable: true,
    hash: 'deadbeef',
    standalone: false,
    fileHash: 'cafebabe',
    content: `---\nname: ${name}\n---\n\n# body`,
    files: [
      { path: 'SKILL.md', size: 2048, hash: 'cafebabe' },
      { path: 'reference.md', size: 512, hash: 'ffee' },
    ],
    sourceRepo: null,
    commitSha: null,
    installedAt: null,
  }
}
