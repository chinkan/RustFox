import { createFileRoute, Link } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'

export const Route = createFileRoute('/_auth/dashboard')({
  component: DashboardPage,
})

function DashboardPage() {
  const { api } = Route.useRouteContext()

  // TanStack Query drives all data fetching. Each query is keyed and cached;
  // switching routes never refetches within `staleTime`.
  const health = useQuery({ queryKey: ['health'], queryFn: api.getHealth })
  const agents = useQuery({ queryKey: ['agents'], queryFn: () => api.listAgents() })
  const workspaces = useQuery({ queryKey: ['workspaces'], queryFn: api.listWorkspaces })
  const tasks = useQuery({ queryKey: ['tasks'], queryFn: api.listTasks })

  const running = agents.data?.filter((a) => a.status === 'running').length ?? 0
  const errored = agents.data?.filter((a) => a.status === 'error').length ?? 0

  return (
    <>
      <div className="page-head">
        <h1>Dashboard</h1>
        <p>System health, agent status and workspace overview.</p>
      </div>

      <section className="section">
        <div className="grid cols-3">
          <Stat
            k="CPU"
            v={health.data ? `${health.data.cpuPercent}%` : '—'}
            sub={health.data ? `uptime ${Math.floor(health.data.uptimeHours / 24)}d ${health.data.uptimeHours % 24}h` : ''}
            bar={health.data?.cpuPercent}
          />
          <Stat
            k="Memory"
            v={health.data ? `${health.data.memUsedGb} / ${health.data.memTotalGb} GB` : '—'}
            sub="host machine"
            bar={health.data ? (health.data.memUsedGb / health.data.memTotalGb) * 100 : undefined}
          />
          <Stat
            k="Disk"
            v={health.data ? `${health.data.diskUsedPercent}%` : '—'}
            sub="1 TB SSD"
            bar={health.data?.diskUsedPercent}
          />
          <Stat
            k="Agents running"
            v={`${running} / ${agents.data?.length ?? 0}`}
            sub={errored > 0 ? `${errored} in error` : 'all healthy'}
          />
          <Stat k="Workspaces" v={String(workspaces.data?.length ?? '—')} sub="active" />
          <Stat
            k="Scheduled tasks"
            v={String(tasks.data?.filter((t) => t.enabled).length ?? '—')}
            sub={`${tasks.data?.length ?? 0} total`}
          />
        </div>
      </section>

      <section className="section">
        <h2 className="section-title">Agents</h2>
        <div className="card" style={{ padding: 0, overflow: 'hidden' }}>
          <table>
            <thead>
              <tr>
                <th>Agent</th>
                <th>Workspace</th>
                <th>Model</th>
                <th>Status</th>
                <th>Last active</th>
              </tr>
            </thead>
            <tbody>
              {agents.data?.map((a) => (
                <tr key={a.id}>
                  <td>
                    <strong>{a.name}</strong>
                  </td>
                  <td className="mono">{a.workspaceId}</td>
                  <td className="mono">{a.model}</td>
                  <td>
                    <span className={`badge dot ${a.status}`}>{a.status}</span>
                  </td>
                  <td className="mono">{fmtTime(a.lastActive)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </section>

      <section className="section">
        <h2 className="section-title">Workspaces</h2>
        <div className="grid cols-2">
          {workspaces.data?.map((w) => (
            <div key={w.id} className="card">
              <div className="row">
                <strong style={{ fontSize: 14 }}>{w.name}</strong>
                <span className="spacer" />
                <span className="badge">{w.agentCount} agents</span>
              </div>
              <p style={{ color: 'var(--text-dim)', fontSize: 13, margin: '8px 0 12px' }}>
                {w.description}
              </p>
              <Link to="/agents" className="btn sm">
                View agents →
              </Link>
            </div>
          ))}
        </div>
      </section>
    </>
  )
}

function Stat({ k, v, sub, bar }: { k: string; v: string; sub?: string; bar?: number }) {
  return (
    <div className="stat">
      <div className="k">{k}</div>
      <div className="v">{v}</div>
      {sub ? <div className="sub">{sub}</div> : null}
      {bar !== undefined ? (
        <div className="bar">
          <span style={{ width: `${Math.min(100, Math.max(0, bar))}%` }} />
        </div>
      ) : null}
    </div>
  )
}

function fmtTime(iso: string): string {
  const d = new Date(iso)
  const diffMin = Math.round((Date.now() - d.getTime()) / 60000)
  if (diffMin < 1) return 'just now'
  if (diffMin < 60) return `${diffMin}m ago`
  if (diffMin < 1440) return `${Math.round(diffMin / 60)}h ago`
  return d.toLocaleDateString()
}
