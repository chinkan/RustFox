import { describe, expect, it, beforeEach } from 'vitest'
import { render, screen, waitFor, fireEvent } from '@testing-library/react'
import { QueryClientProvider, QueryClient } from '@tanstack/react-query'
import { RouterProvider, createRouter, createMemoryHistory } from '@tanstack/react-router'
import { routeTree } from './routeTree.gen'
import { api } from './api/client'
import { authStore, type RouterContext } from './auth'

/**
 * Integration tests: mount the real router (memory history) and assert that
 * navigation, auth guards and type-safe search params actually work at runtime
 * — not just at compile time.
 */

function makeRouter(initialPath = '/') {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  })
  return createRouter({
    routeTree,
    context: { auth: authStore, api, queryClient } satisfies RouterContext,
    history: createMemoryHistory({ initialEntries: [initialPath] }),
    defaultPreload: false,
  })
}

function renderAt(path: string) {
  const router = makeRouter(path)
  render(
    <QueryClientProvider client={router.options.context.queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  )
  return router
}

beforeEach(() => {
  authStore.logout()
  localStorage.clear()
})

describe('auth guard', () => {
  it('redirects an unauthenticated visitor from /dashboard to /login', async () => {
    const router = renderAt('/dashboard')
    await waitFor(() => {
      expect(router.state.location.pathname).toBe('/login')
    })
    expect(await screen.findByText(/Sign in to manage workspaces/i)).toBeTruthy()
  })

  it('lets an authenticated user reach the dashboard', async () => {
    authStore.login('kan', 'admin')
    const router = renderAt('/dashboard')
    await waitFor(() => {
      expect(router.state.location.pathname).toBe('/dashboard')
    })
    expect(await screen.findByRole('heading', { name: 'Dashboard' })).toBeTruthy()
  })
})

describe('RBAC guard', () => {
  it('blocks a non-admin from /settings', async () => {
    authStore.login('guest', 'user')
    const router = renderAt('/settings')
    await waitFor(() => {
      expect(router.state.location.pathname).toBe('/dashboard')
    })
  })

  it('allows an admin into /settings', async () => {
    authStore.login('kan', 'admin')
    const router = renderAt('/settings')
    await waitFor(() => {
      expect(router.state.location.pathname).toBe('/settings')
    })
    expect(await screen.findByText(/Providers/)).toBeTruthy()
  })
})

describe('type-safe search params', () => {
  it('reads validated search params from the URL', async () => {
    authStore.login('kan', 'admin')
    const router = renderAt('/agents?q=exp&status=running&sort=name')

    await waitFor(() => {
      expect(router.state.location.search).toMatchObject({
        q: 'exp',
        status: 'running',
        sort: 'name',
      })
    })

    // Fixtures: exp-executor is `running`, exp-explorer is `idle`.
    // With status=running only the former should survive the filter.
    expect(await screen.findByText('exp-executor')).toBeTruthy()
    expect(screen.queryByText('exp-explorer')).toBeNull()
    expect(screen.queryByText('news-fetcher')).toBeNull()
  })

  it('updates the URL when the user types in the search box', async () => {
    authStore.login('kan', 'admin')
    const router = renderAt('/agents')

    const input = await screen.findByPlaceholderText(/Search agents/i)
    fireEvent.change(input, { target: { value: 'verifier' } })

    await waitFor(() => {
      expect(router.state.location.search).toMatchObject({ q: 'verifier' })
    })
    expect(await screen.findByText('verifier')).toBeTruthy()
  })

  it('falls back to defaults for invalid search params', async () => {
    authStore.login('kan', 'admin')
    const router = renderAt('/agents?status=bogus&q=')
    await waitFor(() => {
      // zod `.catch()` restores the default instead of crashing the route.
      expect(router.state.location.search).toMatchObject({ status: 'all' })
    })
  })
})

describe('nested layout persistence', () => {
  it('renders the chat sidebar and the selected thread together', async () => {
    authStore.login('kan', 'admin')
    const router = renderAt('/chat/t2')
    await waitFor(() => {
      expect(router.state.location.pathname).toBe('/chat/t2')
    })
    // Sidebar (from the layout route) + thread title (from the child route).
    expect(await screen.findByText('TanStack Router for the portal')).toBeTruthy()
    expect(await screen.findByText(/Weekly vault review/)).toBeTruthy()
  })

  it('renders the notFound component when the loader throws notFound()', async () => {
    authStore.login('kan', 'admin')
    renderAt('/chat/does-not-exist')

    // `notFound()` bubbles to the nearest notFoundComponent (defined on __root).
    expect(await screen.findByText(/404 — Route not found/i)).toBeTruthy()
  })
})
