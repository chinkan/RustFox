import { useSyncExternalStore } from 'react'
import type { Api } from '../api/client'

/**
 * Restart detector + reconnect state machine (ADR 0008B).
 *
 * Polls the PUBLIC `GET /api/health` while the tab is visible. A changed
 * `bootId` ⇒ the server restarted ⇒
 *   1. re-`GET /api/auth/me` (the stateless cookie re-authenticates),
 *   2. invalidate every query cache,
 *   3. bump `reconnectEpoch` so chat views reconcile history from the DB
 *      (never auto-replay a POST /api/chat).
 * While the server is unreachable, `online` is false and the shell shows a
 * sticky "restarting…" banner.
 */

export interface BootState {
  online: boolean
  bootId: string | null
  /** Increments once per detected restart — safe effect dependency. */
  reconnectEpoch: number
}

type Listener = () => void

export class BootWatcher {
  private state: BootState = { online: true, bootId: null, reconnectEpoch: 0 }
  private listeners = new Set<Listener>()
  private timer: ReturnType<typeof setInterval> | null = null
  private started = false

  constructor(
    private api: Api,
    private onRestart: (bootId: string) => void,
    private intervalMs = 15_000,
  ) {}

  start() {
    if (this.started) return
    this.started = true
    void this.tick()
    this.timer = setInterval(() => void this.tick(), this.intervalMs)
    document.addEventListener('visibilitychange', this.onVisibility)
  }

  stop() {
    if (this.timer) clearInterval(this.timer)
    this.timer = null
    this.started = false
    document.removeEventListener('visibilitychange', this.onVisibility)
  }

  /** Poll immediately when the tab comes back (phone resume case). */
  private onVisibility = () => {
    if (document.visibilityState === 'visible') void this.tick()
  }

  async tick() {
    try {
      const health = await this.api.getHealth()
      const first = this.state.bootId === null
      const changed = !first && health.bootId !== this.state.bootId
      const wasOffline = !this.state.online
      this.setState({ ...this.state, online: true, bootId: health.bootId })
      if (changed) {
        this.setState({ ...this.state, reconnectEpoch: this.state.reconnectEpoch + 1 })
        this.onRestart(health.bootId)
      } else if (wasOffline && this.state.reconnectEpoch === 0) {
        // Recovered before bootId differed (server bounced fast) — still
        // worth a re-auth + reconcile pass once.
        this.setState({ ...this.state, reconnectEpoch: 1 })
        this.onRestart(health.bootId)
      }
    } catch {
      if (this.state.online) this.setState({ ...this.state, online: false })
    }
  }

  private setState(next: BootState) {
    this.state = next
    this.listeners.forEach((l) => l())
  }

  subscribe = (l: Listener) => {
    this.listeners.add(l)
    return () => this.listeners.delete(l)
  }

  getSnapshot = () => this.state
}

/** React hook view of a BootWatcher (stable snapshot object per change). */
export function useBootState(watcher: BootWatcher | null): BootState {
  const fallback: BootState = { online: true, bootId: null, reconnectEpoch: 0 }
  return useSyncExternalStore(
    watcher ? watcher.subscribe : () => () => undefined,
    watcher ? watcher.getSnapshot : () => fallback,
    () => fallback,
  )
}
