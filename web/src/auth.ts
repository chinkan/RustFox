import { useSyncExternalStore } from 'react'
import type { QueryClient } from '@tanstack/react-query'
import type { Api } from './api/client'

/**
 * Auth + router context.
 *
 * The prototype keeps the session in memory + localStorage so the auth guard
 * (`beforeLoad`) is demonstrable without a backend. Replace the bodies of
 * `login` / `logout` with real calls to the RustFox Axum JWT endpoints.
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
  login: (username: string, role: Role) => void
  logout: () => void
}

export interface RouterContext {
  auth: AuthState
  api: Api
  queryClient: QueryClient
}

const STORAGE_KEY = 'rustfox.portal.session'

function loadSession(): Session | null {
  try {
    const raw = localStorage.getItem(STORAGE_KEY)
    return raw ? (JSON.parse(raw) as Session) : null
  } catch {
    return null
  }
}

function saveSession(session: Session | null): void {
  try {
    if (session) localStorage.setItem(STORAGE_KEY, JSON.stringify(session))
    else localStorage.removeItem(STORAGE_KEY)
  } catch {
    /* storage unavailable — prototype still works in-memory */
  }
}

// ---------------------------------------------------------------------------
// Live store (module-level, subscribe-able)
// ---------------------------------------------------------------------------

let currentSession: Session | null = loadSession()
const listeners = new Set<() => void>()

function emit() {
  listeners.forEach((l) => l())
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
  login(username: string, role: Role) {
    currentSession = { username, role }
    saveSession(currentSession)
    emit()
  },
  logout() {
    currentSession = null
    saveSession(null)
    emit()
  },
}

/** Subscribe a component to auth changes. */
export function useAuth(): AuthState {
  return useSyncExternalStore(
    (cb) => {
      listeners.add(cb)
      return () => listeners.delete(cb)
    },
    () => authStore,
    () => authStore,
  )
}

/** Register an extra listener (used by main.tsx to invalidate the router). */
export function onAuthChange(cb: () => void): () => void {
  listeners.add(cb)
  return () => listeners.delete(cb)
}
