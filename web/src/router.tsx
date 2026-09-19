import { QueryClient } from '@tanstack/react-query'
import { createRouter } from '@tanstack/react-router'
import { routeTree } from './routeTree.gen'
import { api } from './api/client'
import { authStore, onAuthChange, type RouterContext } from './auth'

/**
 * Router + QueryClient singletons.
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

export const router = createRouter({
  routeTree,
  context: {
    auth: authStore,
    api,
    queryClient,
  } satisfies RouterContext,
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

// Re-evaluate route guards (beforeLoad) whenever the session changes.
onAuthChange(() => {
  void router.invalidate()
})
