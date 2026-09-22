/**
 * Shared API types — mirror `docs/portal-api.md` and the Rust handlers in
 * `src/portal/`. Wire format is camelCase (serde renames on the backend).
 */

// ---------------------------------------------------------------------------
// Auth
// ---------------------------------------------------------------------------

export interface MeResponse {
  authenticated: boolean
  username: string
  role: string
}

export interface LoginResponse {
  username: string
  role: string
}

// ---------------------------------------------------------------------------
// Chat
// ---------------------------------------------------------------------------

export interface ChatMessage {
  id: string
  role: 'user' | 'assistant'
  content: string
  createdAt: string | null
}

export interface ChatHistory {
  conversationId: string
  messages: ChatMessage[]
}

export interface Thread {
  /** Conversation UUID (server mints `id` — do not assume short ids). */
  id: string
  title: string
  messageCount: number
  updatedAt: string
}

/** SSE frames emitted by POST /api/chat (docs/portal-api.md → Chat). */
export type StreamFrame =
  | { event: 'token'; data: { delta: string } }
  | { event: 'tool'; data: { name?: string; status: string; success?: boolean } }
  | { event: 'done'; data: { content: string } }
  | { event: 'error'; data: { message: string } }
  | { event: 'ping'; data: null }

// ---------------------------------------------------------------------------
// Agents & skills
// ---------------------------------------------------------------------------

export type AgentStatus = 'idle' | 'running' | 'error'

export interface Agent {
  id: string
  name: string
  model: string
  status: AgentStatus
  /** ISO8601 or null (subagents never report activity yet). */
  lastActive: string | null
  /** "telegram+web" for the main agent, "subagent" for definitions. */
  platform: string
}

export interface Skill {
  name: string
  description: string
  kind: 'skill' | 'agent'
}

export interface ReloadResult {
  skillsLoaded: number
  agentsLoaded: number
}

// ---------------------------------------------------------------------------
// Memory
// ---------------------------------------------------------------------------

export type MemoryKind = 'fact' | 'knowledge' | 'conversation'

export interface MemoryEntry {
  id: string
  kind: MemoryKind
  text: string
  score: number
  /** Currently always null — knowledge rows have no timestamp column. */
  createdAt: string | null
}

// ---------------------------------------------------------------------------
// Tasks
// ---------------------------------------------------------------------------

export interface ScheduledTask {
  id: string
  name: string
  /** 6-field cron for recurring, "once" for one-shot. */
  cron: string
  enabled: boolean
  nextRun: string
}

export interface TaskRun {
  id: number
  runAt: string
  status: 'running' | 'completed' | 'failed' | string
  error: string | null
  response: string | null
}

export interface TaskToggleResult {
  ok: boolean
  id: string
  enabled: boolean
  /** Present on enable — the job re-arms on next restart (MVP limitation). */
  restartToSchedule?: boolean
}

// ---------------------------------------------------------------------------
// Dashboard
// ---------------------------------------------------------------------------

export interface SystemHealth {
  cpuPercent: number
  memUsedGb: number
  memTotalGb: number
  diskUsedPercent: number
  uptimeHours: number
  /** Random per-process id — change ⇒ restart happened (ADR 0008B). */
  bootId: string
}

export interface Stats {
  model: string
  providers: string[]
  messageCount: number
  conversationCount: number
  activeTasks: number
  skills: number
  portalEnabled: boolean
}

// ---------------------------------------------------------------------------
// Settings (ADR 0006 — whitelisted projection, never raw TOML)
// ---------------------------------------------------------------------------

export interface SettingsEditable {
  model: string
  generalLocation: string
  defaultAutonomyMode: string
  portalPort: number
}

export interface ProviderInfo {
  name: string
  model: string
  baseUrl: string
  apiKeyMasked: string | null
}

export interface SettingsMasked {
  telegramBotToken: string
  openrouterApiKey: string
  providers: ProviderInfo[]
  embeddingApiKey?: string | null
  mcpServers: Array<{ name: string }>
}

export interface Settings {
  editable: SettingsEditable
  masked: SettingsMasked
  /** Fields whose change needs a restart before they take effect. */
  restartRequired: string[]
}

export type AutonomyMode = 'default' | 'autopilot' | 'plan'

export interface SettingsPatch {
  model?: string
  generalLocation?: string
  defaultAutonomyMode?: AutonomyMode
  portalPort?: number
}

export interface SettingsPatchResult {
  updated: string[]
  restartRequired: string[]
  applied?: Record<string, string>
}

export interface SoulFile {
  name: string
  content: string
  mtime: string
}

export type SoulName = 'SOUL.md' | 'USER.md' | 'AGENTS.md' | 'MEMORY.md'
