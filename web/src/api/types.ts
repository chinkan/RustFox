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

export interface ScheduleTimezoneInfo {
  timezone: string
  utcOffset: string
  source: string
}

export interface ScheduledTask {
  id: string
  name: string
  /** 6-field cron for recurring, "once" for one-shot. */
  cron: string
  enabled: boolean
  nextRun: string
  // Full editable state (T3): the edit form needs prompt + trigger verbatim.
  prompt: string
  triggerType: 'recurring' | 'one_shot' | string
  triggerValue: string
  status: string
  platform: string
  /** IANA timezone for this task (ADR-0022). */
  timezone: string
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
  /**
   * A real enable re-arms the live scheduler and returns the new job id +
   * computed next-run; disable returns jobRemoved. No `restartToSchedule`
   * fib (T3). See TaskEnableResult / disable shape below.
   */
  schedulerJobId?: string
  nextRun?: string | null
  jobRemoved?: boolean
}
// NOTE: scheduled-task CRUD types live in the Control-plane section below.

// ---------------------------------------------------------------------------
// Control plane (ADR 0011) — skills/agents editing, GitHub installer
// ---------------------------------------------------------------------------

export type Provenance = 'bundled' | 'installed' | 'user'
export type EntryKind = 'skill' | 'agent'

/** Row of GET /api/skills (richer than the legacy /agents/skills list). */
export interface SkillEntry {
  name: string
  kind: EntryKind
  provenance: Provenance
  /** Drifted from the lock/ledger baseline hash. */
  modified: boolean
  deletable: boolean
}

/** Detail of GET /api/skills/{name} · /api/agents/{name}. */
export interface EntryDetail extends SkillEntry {
  hash: string | null
  standalone: boolean
  fileHash: string | null
  content: string
  files: Array<{ path: string; size: number; hash: string | null }>
  sourceRepo: string | null
  commitSha: string | null
  installedAt: string | null
  /** Agents only: frontmatter tools / model. */
  tools?: string[]
  model?: string | null
}

/** Agents detail = EntryDetail plus the frontmatter fields already optional there. */
export type AgentDetail = EntryDetail

export interface FileContent {
  path: string
  content: string
  size: number
}

/** Body of PUT /api/{kind}s/{name}/file — baseHash = optimistic lock (R4). */
export interface FileWriteBody {
  path: string
  content: string
  baseHash?: string | null
}

export interface FileWriteResult {
  ok: boolean
  name: string
  path: string
  bytes: number
  provenance: Provenance
  skillsLoaded?: number
  agentsLoaded?: number
  warnings: string[]
}

/** Body of POST /api/skills (content = full SKILL.md) · POST /api/agents
 *  (frontmatter rendered server-side from these fields). */
export interface CreateEntryBody {
  name: string
  content: string
  description?: string
  model?: string
  tools?: string[]
  maxIterations?: number
  skipBootstrap?: boolean
}

export interface CreateEntryResult {
  ok: boolean
  name: string
  kind: EntryKind
  path: string
  bytes: number
  provenance: Provenance
  skillsLoaded: number
  agentsLoaded: number
  warnings: string[]
}

export interface QuarantineResult {
  ok: boolean
  name: string
  kind: EntryKind
  quarantinedTo: string | null
  skillsLoaded: number
  agentsLoaded: number
}

// ---------------------------------------------------------------------------
// GitHub skill installer (POST /api/skills/install)
// ---------------------------------------------------------------------------

export interface InstallRequest {
  /** owner/repo[:subpath]@ref */
  source: string
  dryRun?: boolean
  /** Exact warning strings from the dry run the owner accepts (R2 gate). */
  acknowledgedWarnings?: string[]
  force?: boolean
}

export interface Refusal {
  skill: string
  file: string
  rule: string
  detail: string
}

export interface InstallWarning {
  skill: string
  file: string
  rule: string
  detail: string
}

export interface InstallVerdict {
  refused: Refusal[]
  warnings: InstallWarning[]
  notes: string[]
}

export interface PlannedSkill {
  name: string
  description: string
  files: Array<{ path: string; size: number }>
  sizeBytes: number
}

export interface InstallDryRunResult {
  dryRun: true
  source: { owner: string; repo: string; ref: string }
  skills: PlannedSkill[]
  verdict: InstallVerdict
}

export interface InstallResult {
  dryRun: false
  installed: string[]
  skipped: Array<{ name: string; reason: string }>
  verdict: InstallVerdict
  reload: { skillsLoaded: number; agentsLoaded: number }
}

/** Unique key a warning must be acknowledged under (server warning_key()). */
export function warningKey(w: InstallWarning): string {
  return `[${w.rule}] ${w.skill}: ${w.file} — ${w.detail}`
}

export interface InstalledLedgerView {
  skills: Record<string, InstalledRecord>
  agents: Record<string, InstalledRecord>
}

export interface InstalledRecord {
  source_repo: string
  commit_sha: string
  ref: string
  installed_at: string
  content_hash: string
}

// ---------------------------------------------------------------------------
// Task CRUD (T3)
// ---------------------------------------------------------------------------

export interface TaskDetail extends ScheduledTask {
  prompt: string
  triggerType: 'recurring' | 'one_shot'
  triggerValue: string
  status: string
  platform: string
}

export interface TaskCreateBody {
  name: string
  prompt: string
  triggerType: 'recurring' | 'one_shot'
  /** 6-field cron (sec min hour dom mon dow) or ISO-8601 for one_shot. */
  triggerValue: string
  /** Optional IANA timezone; omit → effective default. */
  timezone?: string
}

export interface TaskUpdateBody {
  name?: string
  prompt?: string
  triggerValue?: string
  /** Rejected with 400 trigger_type_immutable if changed. */
  triggerType?: 'recurring' | 'one_shot'
  timezone?: string
}

export interface TaskCreateResult {
  id: string
  name: string
  schedulerJobId: string
  nextRun: string | null
}

export interface TaskUpdateResult {
  id: string
  updated: { name: string; prompt: string; triggerValue: string }
  rearmed: boolean
  schedulerJobId: string | null
  nextRun: string | null
}

export interface TaskDeleteResult {
  ok: boolean
  id: string
  softDeleted: boolean
  historyPreserved: boolean
}

/** A real enable re-arms the live scheduler — no restartToSchedule fib. */
export interface TaskEnableResult {
  ok: boolean
  id: string
  enabled: boolean
  schedulerJobId: string
  nextRun: string | null
}

/** Heuristic 5- or 6-field cron check (the server gate is the real parser). */
export const CRON_5_OR_6_FIELD = /^(\S+\s+){4,5}\S+$/

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

/** ADR-0041 — masked MCP server projection (no secret values). */
export type McpServerKind = 'preset' | 'custom' | 'stdio'

export interface McpServerMasked {
  name: string
  kind: McpServerKind
  /** Preset id when kind=preset (exa / jina). */
  preset?: string | null
  enabled: boolean
  url?: string | null
  authConfigured: boolean
  headerNames: string[]
  connected: boolean
  privacyAccepted?: boolean | null
  privacyAcceptedAt?: number | null
  /** Hand-written preset row diverges from pinned URL — portal read-only. */
  configOwned: boolean
}

/** Static catalog entry from GET /api/settings/mcp/presets. */
export interface McpPreset {
  id: string
  label: string
  kind: string
  url: string
  auth: string
  authHeader?: string
  tools: string[]
  keylessDefault?: boolean
  requiresPrivacyConsent?: boolean
  description?: string
}

export interface McpPresetsResponse {
  presets: McpPreset[]
}

/** PUT /api/settings/mcp/{name} body (all optional except Custom url on create). */
export interface McpUpsertBody {
  name?: string
  url?: string
  enabled?: boolean
  bearer?: string
  headers?: Record<string, string>
  privacyAccepted?: boolean
  clearApiKey?: boolean
}

export interface McpServerMutationResult {
  server: McpServerMasked
}

export interface SettingsMasked {
  telegramBotToken: string
  openrouterApiKey: string
  /** ADR-0044 — masked xAI key when a type=xai provider exists (else placeholder). */
  xaiApiKey?: string
  providers: ProviderInfo[]
  embeddingApiKey?: string | null
  mcpServers: McpServerMasked[]
}

/** ADR 0011 R7 — system-prompt provenance projection. */
export interface SystemPromptInfo {
  /** Which layer is actually feeding the live prompt. */
  source: 'file' | 'inline' | 'builtin'
  /** Configured pointer (null when the operator hasn't set one). */
  pointer: string | null
  /** Both file pointer AND a customised inline prompt are set — the file
   *  wins, so the inline copy is dead weight. UI must warn. */
  divergence: boolean
}


export type SandboxMode = 'sandbox' | 'trusted'

export interface SandboxStatus {
  mode: SandboxMode
  platform: string
  windowsNote?: string | null
  canInstall: boolean
  installButton: string
  installingHint: string
  installCommand?: string | null
  switchToSafeLabel: string
  /** AppArmor / userns blocked the probe — show system-settings copy only. */
  blockedBySystem?: boolean
  /** Optional status copy (blocked-by-system / unsupported PM). */
  statusMessage?: string | null
}

export interface SandboxSwitchResult {
  ok: boolean
  mode: SandboxMode
  message: string
  installCommand?: string
  canInstall?: boolean
  includesInstallCommand?: boolean
  installingHint?: string
  detail?: string
}

/** Portal Settings self-upgrade choice (ADR-0030). No free-text branches. */
export type SelfUpgradeChoice = 'auto' | 'main' | 'release'

export interface SelfUpgradeResult {
  ok: boolean
  mode: string
  log: string
}

/**
 * GET /api/opencode/status (ADR-0043 Phase 1).
 * Snake_case on the wire — same JSON as the public wizard route. Detect-only:
 * not an Active provider and not a chat route.
 */
export type OpenCodeState = 'absent' | 'no_creds' | 'ready'

export interface OpenCodeStatus {
  state: OpenCodeState
  model_count: number
  models: string[]
  message: string | null
}


export interface Settings {
  editable: SettingsEditable
  masked: SettingsMasked
  /** Fields whose change needs a restart before they take effect. */
  restartRequired: string[]
  systemPrompt: SystemPromptInfo
  sandbox: SandboxStatus
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

export type SoulName = 'SOUL.md' | 'USER.md' | 'AGENTS.md' | 'MEMORY.md' | 'system'

// ---------------------------------------------------------------------------
// Secrets (Slice 2)
// ---------------------------------------------------------------------------

export interface SecretClaimMeta {
  id: string
  name: string
  expiresAt: number
  status: string
}

export interface SecretClaimResult {
  ok: boolean
  name: string
  stored: boolean
}

export interface PendingSecret {
  id: string
  name: string
  expiresAt: number
}
