import { createFileRoute } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'
import { z } from 'zod'

/**
 * Type-safe search params — the TanStack Router killer feature.
 *
 * `validateSearch` parses + validates the URL. `Route.useSearch()` then
 * returns a *fully inferred* type, and `<Link search={...}>` is checked at
 * compile time. The URL stays shareable / bookmarkable, and every filter is
 * in the address bar.
 */
const agentSearchSchema = z.object({
  q: z.string().default('').catch(''),
  status: z.enum(['all', 'idle', 'running', 'error']).default('all').catch('all'),
  sort: z.enum(['name', 'status']).default('name').catch('name'),
})

export const Route = createFileRoute('/_auth/agents')({
  validateSearch: agentSearchSchema,
  component: AgentsPage,
})

function AgentsPage() {
  const { api } = Route.useRouteContext()
  const { q, status, sort } = Route.useSearch()
  const navigate = Route.useNavigate()

  const agents = useQuery({ queryKey: ['agents'], queryFn: () => api.listAgents() })

  const filtered = (agents.data ?? [])
    .filter((a) => (status === 'all' ? true : a.status === status))
    .filter((a) => a.name.toLowerCase().includes(q.toLowerCase()))
    .sort((a, b) =>
      sort === 'name' ? a.name.localeCompare(b.name) : a.status.localeCompare(b.status),
    )

  return (
    <>
      <div className="page-head">
        <h1>Agents</h1>
        <p>
          Every filter lives in the URL — copy the address bar to share this exact
          view.
        </p>
      </div>

      <div className="row" style={{ marginBottom: 18 }}>
        <input
          type="search"
          placeholder="Search agents…"
          value={q}
          onChange={(e) =>
            // Functional update preserves the other search params.
            void navigate({ search: (prev) => ({ ...prev, q: e.target.value }) })
          }
          style={{ width: 220 }}
        />

        <select
          value={status}
          onChange={(e) =>
            void navigate({
              search: (prev) => ({ ...prev, status: e.target.value as typeof status }),
            })
          }
        >
          <option value="all">All statuses</option>
          <option value="running">Running</option>
          <option value="idle">Idle</option>
          <option value="error">Error</option>
        </select>

        <select
          value={sort}
          onChange={(e) =>
            void navigate({
              search: (prev) => ({ ...prev, sort: e.target.value as typeof sort }),
            })
          }
        >
          <option value="name">Sort: name</option>
          <option value="status">Sort: status</option>
        </select>

        <span className="spacer" />
        <span className="badge">{filtered.length} shown</span>
      </div>

      <div className="card" style={{ padding: 0, overflow: 'hidden' }}>
        <table>
          <thead>
            <tr>
              <th>Agent</th>
              <th>Workspace</th>
              <th>Model</th>
              <th>Status</th>
            </tr>
          </thead>
          <tbody>
            {filtered.map((a) => (
              <tr key={a.id}>
                <td>
                  <strong>{a.name}</strong>
                </td>
                <td className="mono">{a.workspaceId}</td>
                <td className="mono">{a.model}</td>
                <td>
                  <span className={`badge dot ${a.status}`}>{a.status}</span>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
        {filtered.length === 0 ? (
          <div className="empty">
            <div className="big">🔍</div>
            <p>No agents match this filter.</p>
          </div>
        ) : null}
      </div>

      <p className="mono" style={{ marginTop: 16 }}>
        Current URL state → q=&quot;{q}&quot; status={status} sort={sort}
      </p>
    </>
  )
}
