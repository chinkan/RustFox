import { QueryClient } from '@tanstack/react-query'
import { createRouter } from '@tanstack/react-router'
import { routeTree } from './routeTree.gen'
import { api } from './api/client'
import { authStore, bindAuthApi, onAuthChange, type RouterContext } from './auth'
import { BootWatcher } from './hooks/useBootWatcher'
import { chatSession } from './hooks/useChatStream'
import './i18n'

/**
 * Router + QueryClient + BootWatcher singletons.
 *
 * Kept out of main.tsx so tests and the app share one configuration.
 */

export const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 30_000,
      refetchOnWindowFocus: false,
      retry: 1,
    },
  },
})

bindAuthApi(api)

/**
 * Restart/reconnect state machine (ADR 0008B). On a detected restart:
 * re-`/auth/me` (stateless cookie re-authenticates), invalidate all query
 * caches, and reconcile the chat from the DB — never auto-replay a send.
 */
export const bootWatcher = new BootWatcher(api, (bootId) => {
  void (async () => {
    await authStore.restore()
    queryClient.invalidateQueries()
    void chatSession(api).reconcile()
    // One retry pass for queries whose fetch failed during the window.
    setTimeout(() => {
      if (!bootWatcher.getSnapshot().online) return
      queryClient.invalidateQueries()
    }, 1500)
    void bootId
  })()
})

/** Start polling (idempotent). Called from main.tsx only in the browser. */
export function startBootWatcher() {
  bootWatcher.start()
}

/**
 * One-shot cookie bootstrap: on first paint ask /auth/me; if authenticated
 * (returning session), seed the store. Unauthenticated stays unauthenticated.
 */
export const restorePromise = authStore.restore()

export const routerContext: RouterContext = {
  auth: authStore,
  api,
  queryClient,
  boot: bootWatcher,
  chat: chatSession(api),
}

export const router = createRouter({
  routeTree,
  context: routerContext,
  defaultPreload: 'intent',
  defaultPreloadStaleTime: 0,
  scrollRestoration: true,
})

// Register the router type globally so <Link> / useNavigate() are fully typed.
declare module '@tanstack/react-router' {
  interface Register {
    router: typeof router
  }
}

// Re-evaluate route guards (beforeLoad) whenever the session changes —
// but not before the initial restore settled (guards await it themselves).
onAuthChange(() => {
  void router.invalidate()
})
