import { createFileRoute, redirect } from '@tanstack/react-router'

/**
 * `/` — redirect to the dashboard (or login if unauthenticated).
 * A `beforeLoad` on the index route keeps the entry point deterministic.
 */
export const Route = createFileRoute('/')({
  beforeLoad: ({ context }) => {
    throw redirect({
      to: context.auth.isAuthenticated ? '/dashboard' : '/login',
    })
  },
})
