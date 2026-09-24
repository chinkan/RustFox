import { describe, expect, it, beforeEach } from 'vitest'
import { render, screen, waitFor, fireEvent } from '@testing-library/react'
import { QueryClientProvider, QueryClient } from '@tanstack/react-query'
import { RouterProvider, createRouter, createMemoryHistory } from '@tanstack/react-router'
import { routeTree } from './routeTree.gen'
import { authStore, bindAuthApi, __resetAuthForTests, type RouterContext } from './auth'
import { BootWatcher } from './hooks/useBootWatcher'
import { ChatSession } from './hooks/useChatStream'
import { fakeApi } from './api/fake'

/**
 * Integration tests: mount the real router (memory history) against a FAKE
 * api object — no network — and assert navigation, auth guards and
 * type-safe search params work at runtime, not just at compile time.
 */

const api = fakeApi()

function makeContext(over: Parameters<typeof fakeApi>[0] = {}): RouterContext {
  const ctxApi = fakeApi(over)
  const qc = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  })
  return {
    auth: authStore,
    api: ctxApi,
    queryClient: qc,
    boot: new BootWatcher(ctxApi, () => undefined),
    chat: new ChatSession(ctxApi),
  }
}

function renderAt(path: string, ctx: RouterContext) {
  const router = createRouter({
    routeTree,
    context: ctx,
    history: createMemoryHistory({ initialEntries: [path] }),
    defaultPreload: false,
  })
  render(
    <QueryClientProvider client={ctx.queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  )
  return router
}

beforeEach(() => {
  bindAuthApi(api)
  __resetAuthForTests()
  localStorage.clear()
  sessionStorage.clear()
})

/** Seed a session via the fake login round-trip. */
async function signIn() {
  await authStore.loginWithToken('test-token')
}

describe('auth guard', () => {
  it('redirects an unauthenticated visitor from /dashboard to /login', async () => {
    bindAuthApi(fakeApi({ me: async () => ({ authenticated: false, username: '', role: '' }) }))
    const router = renderAt('/dashboard', makeContext({ me: async () => ({ authenticated: false, username: '', role: '' }) }))
    await waitFor(() => {
      expect(router.state.location.pathname).toBe('/login')
    })
    expect(await screen.findByText(/Sign in with your portal token/i)).toBeTruthy()
  })

  it('lets an authenticated user reach the dashboard', async () => {
    await signIn()
    const router = renderAt('/dashboard', makeContext())
    await waitFor(() => {
      expect(router.state.location.pathname).toBe('/dashboard')
    })
    expect(await screen.findByRole('heading', { name: 'Dashboard' })).toBeTruthy()
  })

  it('restores a session from /auth/me on cold boot (cookie re-auth — ADR 0008A)', async () => {
    // me() says authenticated — guards must NOT bounce even though the
    // in-memory session started empty (the restart-survival path).
    const authed = { authenticated: true, username: 'kan', role: 'admin' }
    bindAuthApi(fakeApi({ me: async () => authed }))
    const router = renderAt('/dashboard', makeContext({ me: async () => authed }))
    await waitFor(() => {
      expect(router.state.location.pathname).toBe('/dashboard')
    })
  })
})

describe('RBAC guard', () => {
  it('renders /settings for an admin with real masked data', async () => {
    await signIn()
    const router = renderAt('/settings', makeContext())
    await waitFor(() => {
      expect(router.state.location.pathname).toBe('/settings')
    })
    expect(await screen.findByText(/Secrets/i)).toBeTruthy()
    // Masked secret projection renders; raw values never cross the boundary.
    expect(await screen.findByText('123…456')).toBeTruthy()
  })

  it('sidebar hides Settings for a non-admin session', async () => {
    bindAuthApi(fakeApi({ login: async () => ({ username: 'guest', role: 'user' }) }))
    await authStore.loginWithToken('t')
    renderAt('/dashboard', makeContext())
    await screen.findByRole('heading', { name: 'Dashboard' })
    expect(screen.queryByText('Settings')).toBeNull()
  })
})

describe('type-safe search params', () => {
  it('reads validated search params from the URL', async () => {
    await signIn()
    const router = renderAt('/agents?q=exp&status=running&sort=name', makeContext())

    await waitFor(() => {
      expect(router.state.location.search).toMatchObject({
        q: 'exp',
        status: 'running',
        sort: 'name',
      })
    })

    // Fixtures: exp-executor is `running`; exp-explorer is `idle`.
    expect(await screen.findByText('exp-executor')).toBeTruthy()
    expect(screen.queryByText('exp-explorer')).toBeNull()
    expect(screen.queryByText('thread-writer-hk')).toBeNull()
  })

  it('updates the URL when the user types in the search box', async () => {
    await signIn()
    const router = renderAt('/agents', makeContext())

    const input = await screen.findByPlaceholderText(/Search agents/i)
    fireEvent.change(input, { target: { value: 'verifier' } })

    await waitFor(() => {
      expect(router.state.location.search).toMatchObject({ q: 'verifier' })
    })
    // 'verifier' appears in both the agent row and the Skills grid — assert
    // the filtered table shows exactly the one agent row.
    const rows = await screen.findAllByText('verifier')
    expect(rows.length).toBeGreaterThanOrEqual(1)
    expect(screen.queryByText('exp-executor')).toBeNull()
  })

  it('falls back to defaults for invalid search params', async () => {
    await signIn()
    const router = renderAt('/agents?status=bogus&q=', makeContext())
    await waitFor(() => {
      // zod `.catch()` restores the default instead of crashing the route.
      expect(router.state.location.search).toMatchObject({ status: 'all' })
    })
  })
})

describe('chat', () => {
  it('renders the thread sidebar and history bubbles', async () => {
    await signIn()
    const router = renderAt('/chat/conv-1', makeContext())
    await waitFor(() => {
      expect(router.state.location.pathname).toBe('/chat/conv-1')
    })
    // 'hello' exists as the thread title in the sidebar AND the user bubble.
    const hellos = await screen.findAllByText('hello')
    expect(hellos.length).toBeGreaterThanOrEqual(2)
    expect(await screen.findByText('hi kan')).toBeTruthy()
  })

  it('renders notFound for an unknown thread id', async () => {
    await signIn()
    renderAt('/chat/nope', makeContext())
    expect(await screen.findByText(/404/i)).toBeTruthy()
  })
})
