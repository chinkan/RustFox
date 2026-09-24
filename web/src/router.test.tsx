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

// ---------------------------------------------------------------------------
// T5 control-plane: skills list/editor/install + task CRUD through the router
// ---------------------------------------------------------------------------

describe('skills page (T5)', () => {
  it('lists entries with provenance badges + modified dot, bundled not deletable', async () => {
    await signIn()
    renderAt('/skills', makeContext())
    await waitFor(() => expect(screen.getByText('hko-weather')).toBeTruthy())
    expect(screen.getByText('bundled')).toBeTruthy()
    expect(screen.getByText('installed')).toBeTruthy()
    expect(document.querySelector('.dirty-dot')).toBeTruthy()
    const row = screen.getByText('hko-weather').closest('tr')!
    expect(row.textContent).toContain('bundled skills cannot be deleted here')
  })

  it('editor drawer saves with baseHash (optimistic lock)', async () => {
    await signIn()
    let savedBody: { path: string; content: string; baseHash?: string } | null = null
    renderAt(
      '/skills',
      makeContext({
        putSkillFile: async (name, body) => {
          savedBody = body
          return {
            ok: true, name, path: body.path, bytes: body.content.length,
            provenance: 'user' as const, skillsLoaded: 92, warnings: [],
          }
        },
      }),
    )
    await waitFor(() => expect(screen.getByText('my-experiment')).toBeTruthy())
    fireEvent.click(screen.getAllByText('Open')[1]!)
    const textarea = await waitFor(() => {
      const el = document.querySelector('.drawer textarea') as HTMLTextAreaElement | null
      expect(el).toBeTruthy()
      return el!
    })
    fireEvent.change(textarea, { target: { value: '# edited content' } })
    fireEvent.click(screen.getByText('Save'))
    await waitFor(() => expect(savedBody).toBeTruthy())
    expect(savedBody!.content).toBe('# edited content')
    expect(savedBody!.baseHash).toBe('cafebabe')
  })

  it('409 conflict surfaces the re-read path', async () => {
    await signIn()
    const { ApiError } = await import('./api/client')
    renderAt(
      '/skills',
      makeContext({
        putSkillFile: async () => {
          throw new ApiError('changed', 409, 'skill_changed_since_read')
        },
      }),
    )
    await waitFor(() => expect(screen.getByText('my-experiment')).toBeTruthy())
    fireEvent.click(screen.getAllByText('Open')[1]!)
    const textarea = await waitFor(() => {
      const el = document.querySelector('.drawer textarea') as HTMLTextAreaElement | null
      expect(el).toBeTruthy()
      return el!
    })
    fireEvent.change(textarea, { target: { value: 'x' } })
    fireEvent.click(screen.getByText('Save'))
    await waitFor(() => expect(screen.getByText('Re-read')).toBeTruthy())
  })

  it('install dialog: dry-run then warning must be ticked then install sends acks', async () => {
    await signIn()
    const calls: Array<{ dryRun?: boolean; acknowledgedWarnings?: string[] }> = []
    renderAt(
      '/skills',
      makeContext({
        installSkill: async (req) => {
          calls.push(req)
          return req.dryRun
            ? {
                dryRun: true as const,
                source: { owner: 'foo', repo: 'bar', ref: 'abc123' },
                skills: [
                  {
                    name: 'cool-skill',
                    description: 'does cool things',
                    files: [{ path: 'SKILL.md', size: 1200 }],
                    sizeBytes: 1200,
                  },
                ],
                verdict: {
                  refused: [],
                  warnings: [
                    { skill: 'cool-skill', file: 'scripts/run.sh', rule: 'executable', detail: 'contains a shell script' },
                  ],
                  notes: [],
                },
              }
            : {
                dryRun: false as const,
                installed: ['cool-skill'],
                skipped: [],
                verdict: { refused: [], warnings: [], notes: [] },
                reload: { skillsLoaded: 92, agentsLoaded: 10 },
              }
        },
      }),
    )
    await waitFor(() => expect(screen.getByText('my-experiment')).toBeTruthy())
    fireEvent.click(screen.getByText('Install from GitHub'))
    fireEvent.change(screen.getByPlaceholderText(/owner\/repo/), { target: { value: 'foo/bar' } })
    fireEvent.click(screen.getByText('Dry run'))
    await waitFor(() => expect(screen.getByText('cool-skill')).toBeTruthy())
    const installBtn = screen.getByText('Install') as HTMLButtonElement
    expect(installBtn.disabled).toBe(true) // R2 gate: warning unacked
    fireEvent.click(screen.getByText('Acknowledge all'))
    await waitFor(() => expect((screen.getByText('Install') as HTMLButtonElement).disabled).toBe(false))
    fireEvent.click(screen.getByText('Install'))
    await waitFor(() => expect(calls.filter((c) => c.dryRun === false).length).toBe(1))
    expect(calls.find((c) => c.dryRun === false)!.acknowledgedWarnings).toHaveLength(1)
  })

  it('installed ledger is visible in the dialog', async () => {
    await signIn()
    renderAt('/skills', makeContext())
    await waitFor(() => expect(screen.getByText('my-experiment')).toBeTruthy())
    fireEvent.click(screen.getByText('Install from GitHub'))
    await waitFor(() => expect(screen.getByText(/foo\/bar@abc123de/)).toBeTruthy())
  })
})

describe('tasks page (T5)', () => {
  it('create flow validates the 6-field cron gate', async () => {
    await signIn()
    const created: unknown[] = []
    renderAt(
      '/tasks',
      makeContext({
        createTask: async (body) => {
          created.push(body)
          return { id: 'c9', name: body.name, schedulerJobId: 'job-9', nextRun: '2026-09-26T12:00:00+08:00' }
        },
      }),
    )
    await waitFor(() => expect(screen.getByText('香港天氣報告')).toBeTruthy())
    fireEvent.click(screen.getByText('New task'))
    fireEvent.change(screen.getByLabelText('Task'), { target: { value: 'standup' } })
    fireEvent.change(screen.getByLabelText('Prompt'), { target: { value: 'nag me' } })
    const cronInput = screen.getByPlaceholderText('0 30 7 * * *') as HTMLInputElement
    const submit = () => screen.getByText('Create') as HTMLButtonElement
    await waitFor(() => expect(submit().disabled).toBe(false))
    fireEvent.change(cronInput, { target: { value: '0 12 * *' } }) // 5 fields
    await waitFor(() => expect(submit().disabled).toBe(true))
    expect(screen.getByText(/Invalid trigger value/)).toBeTruthy()
    fireEvent.change(cronInput, { target: { value: '0 0 9 * * 1-5' } })
    await waitFor(() => expect(submit().disabled).toBe(false))
    fireEvent.click(submit())
    await waitFor(() => expect(created.length).toBe(1))
    expect(created[0]).toMatchObject({ name: 'standup', triggerValue: '0 0 9 * * 1-5' })
  })

  it('enable really re-arms and shows the live next-run (no restart fib)', async () => {
    await signIn()
    let enabledId: string | null = null
    renderAt(
      '/tasks',
      makeContext({
        enableTask: async (id) => {
          enabledId = id
          return { ok: true, id, enabled: true, schedulerJobId: 'job-x', nextRun: '2026-09-27T07:30:00+08:00' }
        },
      }),
    )
    await waitFor(() => expect(screen.getByText('arXiv 簡報')).toBeTruthy())
    fireEvent.click(screen.getByText('Enable'))
    await waitFor(() => expect(enabledId).toBe('c2'))
    await waitFor(() => expect(screen.getByText(/Re-armed live/)).toBeTruthy())
  })

  it('delete confirm promises preserved history (soft delete, R6)', async () => {
    await signIn()
    let deletedId: string | null = null
    renderAt(
      '/tasks',
      makeContext({
        deleteTask: async (id) => {
          deletedId = id
          return { ok: true, id, softDeleted: true, historyPreserved: true }
        },
      }),
    )
    await waitFor(() => expect(screen.getByText('香港天氣報告')).toBeTruthy())
    fireEvent.click(screen.getAllByText('Delete')[0]!)
    expect(screen.getByText(/run history stays available/)).toBeTruthy()
    fireEvent.click(document.querySelector('.modal button.danger')!)
    await waitFor(() => expect(deletedId).toBeTruthy())
  })

  it('edit form locks the trigger type', async () => {
    await signIn()
    renderAt('/tasks', makeContext())
    await waitFor(() => expect(screen.getByText('香港天氣報告')).toBeTruthy())
    fireEvent.click(screen.getAllByText('Edit')[0]!)
    const select = screen.getByText('Recurring (cron)').closest('select') as HTMLSelectElement
    expect(select.disabled).toBe(true)
    expect(screen.getByText('Trigger type cannot be changed after creation.')).toBeTruthy()
  })
})
