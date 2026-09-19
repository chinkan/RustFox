/**
 * Shared API types — these mirror what the RustFox Axum backend will return.
 * Kept in one place so the TanStack Router loaders / Query hooks stay typed.
 */

export interface Workspace {
  id: string
  name: string
  description: string
  agentCount: number
  updatedAt: string
}

export type AgentStatus = 'idle' | 'running' | 'error'

export interface Agent {
  id: string
  workspaceId: string
  name: string
  model: string
  status: AgentStatus
  lastActive: string
}

export interface Thread {
  id: string
  workspaceId: string
  title: string
  messageCount: number
  updatedAt: string
}

export interface ChatMessage {
  id: string
  role: 'user' | 'assistant'
  content: string
  createdAt: string
}

export interface MemoryEntry {
  id: string
  kind: 'fact' | 'knowledge' | 'conversation'
  text: string
  score: number
  createdAt: string
}

export interface ScheduledTask {
  id: string
  name: string
  cron: string
  enabled: boolean
  nextRun: string
}

export interface SystemHealth {
  cpuPercent: number
  memUsedGb: number
  memTotalGb: number
  diskUsedPercent: number
  uptimeHours: number
}
