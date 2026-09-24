import { createFileRoute, redirect } from '@tanstack/react-router'
import { ensureAuthBootstrapped } from '../auth'

/**
 * `/` — redirect to the dashboard (or login if unauthenticated).
 * Awaits the cookie-restore round-trip first so a returning session never
 * sees the login screen flash (the guard runs before authStore.restore
 * resolves on cold boot otherwise).
 */
export const Route = createFileRoute('/')({
  beforeLoad: async ({ context }) => {
    await ensureAuthBootstrapped()
    throw redirect({
      to: context.auth.isAuthenticated ? '/dashboard' : '/login',
    })
  },
})
