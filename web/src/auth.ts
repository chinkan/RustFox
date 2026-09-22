import { useSyncExternalStore } from 'react'
import type { QueryClient } from '@tanstack/react-query'
import type { Api } from './api/client'
import type { BootWatcher } from './hooks/useBootWatcher'
import type { ChatSession } from './hooks/useChatStream'

/**
 * Auth store backed by the real backend session (ADR 0008A).
 *
 * The session lives in an HttpOnly signed cookie — this store only mirrors
 * the *display* fields returned by `GET /api/auth/me`. `restore()` runs once
 * at boot and again whenever the boot watcher detects a restart, so a
 * restart never logs the user out: the cookie re-authenticates transparently.
 */

export type Role = 'admin' | 'user'

export interface Session {
  username: string
  role: Role
}

export interface AuthState {
  session: Session | null
  isAuthenticated: boolean
  isAdmin: boolean
  /** Sign in via POST /api/auth/login (server sets the HttpOnly cookie). */
  loginWithToken: (token: string) => Promise<void>
  /** Drop the local session + fire-and-forget server logout. */
  logout: (everywhere?: boolean) => void
  /** Re-read /api/auth/me (deduplicated); resolves with the new session. */
  restore: () => Promise<Session | null>
}

export interface RouterContext {
  auth: AuthState
  api: Api
  queryClient: QueryClient
  boot: BootWatcher
  chat: ChatSession
}

let currentSession: Session | null = null
let restoreInFlight: Promise<Session | null> | null = null
let restoredOnce = false
const listeners = new Set<() => void>()

function emit() {
  listeners.forEach((l) => l())
}

// Late-bound so router.tsx (and tests) own the single api instance.
let boundApi: Api | null = null
export function bindAuthApi(api: Api) {
  boundApi = api
}
function needApi(): Api {
  if (!boundApi) throw new Error('auth store used before bindAuthApi()')
  return boundApi
}

async function doRestore(): Promise<Session | null> {
  try {
    const me = await needApi().me()
    currentSession = me.authenticated
      ? { username: me.username, role: me.role === 'admin' ? 'admin' : 'user' }
      : null
  } catch {
    // Network blip during a restart window: keep the last known session —
    // the signed cookie is still valid, the poller will settle it.
  }
  restoredOnce = true
  emit()
  return currentSession
}

export const authStore: AuthState = {
  get session() {
    return currentSession
  },
  get isAuthenticated() {
    return currentSession !== null
  },
  get isAdmin() {
    return currentSession?.role === 'admin'
  },
  async loginWithToken(token: string) {
    const res = await needApi().login(token)
    currentSession = { username: res.username, role: (res.role as Role) || 'admin' }
    restoredOnce = true
    emit()
  },
  logout(everywhere = false) {
    currentSession = null
    restoredOnce = true
    emit()
    // Fire-and-forget: clearing the local session is what the UI needs;
    // server-side cookie invalidation must not block navigation.
    void needApi()
      .logout(everywhere)
      .catch(() => undefined)
  },
  restore() {
    if (!restoreInFlight) {
      restoreInFlight = doRestore().finally(() => {
        restoreInFlight = null
      })
    }
    return restoreInFlight
  },
}

/** True once a /auth/me round-trip has completed (guards boot races). */
export function isRestored(): boolean {
  return restoredOnce
}

/** Awaited by route guards before evaluating the auth check. */
export async function ensureAuthBootstrapped(): Promise<void> {
  if (!restoredOnce) await authStore.restore()
}

/** Subscribe a component to auth changes. */
export function useAuth(): AuthState {
  return useSyncExternalStore(
    (cb) => {
      listeners.add(cb)
      return () => {
        listeners.delete(cb)
      }
    },
    () => authStore,
    () => authStore,
  )
}

/** Register an extra listener (used by router.tsx to invalidate the router). */
export function onAuthChange(cb: () => void): () => void {
  listeners.add(cb)
  return () => {
    listeners.delete(cb)
  }
}

/** Test hook: reset module state between vitest cases. */
export function __resetAuthForTests() {
  currentSession = null
  restoredOnce = false
  restoreInFlight = null
}
