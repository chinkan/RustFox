import { createFileRoute } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'

export const Route = createFileRoute('/_auth/dashboard')({
  component: DashboardPage,
})

function DashboardPage() {
  const { api } = Route.useRouteContext()
  const { t } = useTranslation()

  // TanStack Query drives all data fetching against the real Axum API.
  const health = useQuery({ queryKey: ['health'], queryFn: api.getHealth })
  const stats = useQuery({ queryKey: ['stats'], queryFn: api.getStats })
  const agents = useQuery({ queryKey: ['agents'], queryFn: api.listAgents })
  const tasks = useQuery({ queryKey: ['tasks'], queryFn: api.listTasks })

  const running = agents.data?.filter((a) => a.status === 'running').length ?? 0
  const errored = agents.data?.filter((a) => a.status === 'error').length ?? 0

  return (
    <>
      <div className="page-head">
        <h1>{t('dashboard.title')}</h1>
        <p>{t('dashboard.subtitle')}</p>
      </div>

      <section className="section">
        <div className="grid cols-3">
          <Stat
            k={t('dashboard.cpu')}
            v={health.data ? `${health.data.cpuPercent}%` : '—'}
            sub={
              health.data
                ? `${t('dashboard.uptime')} ${Math.floor(health.data.uptimeHours / 24)}d ${Math.round(health.data.uptimeHours % 24)}h`
                : ''
            }
            bar={health.data?.cpuPercent}
          />
          <Stat
            k={t('dashboard.memory')}
            v={
              health.data
                ? `${health.data.memUsedGb} / ${health.data.memTotalGb} GB`
                : '—'
            }
            sub={t('dashboard.host')}
            bar={
              health.data && health.data.memTotalGb > 0
                ? (health.data.memUsedGb / health.data.memTotalGb) * 100
                : undefined
            }
          />
          <Stat
            k={t('dashboard.disk')}
            v={health.data ? `${health.data.diskUsedPercent}%` : '—'}
            bar={health.data?.diskUsedPercent}
          />
          <Stat
            k={t('dashboard.agentsRunning')}
            v={`${running} / ${agents.data?.length ?? '—'}`}
            sub={errored > 0 ? t('dashboard.inError', { count: errored }) : t('dashboard.allHealthy')}
          />
          <Stat
            k={t('dashboard.skills')}
            v={stats.data ? String(stats.data.skills) : '—'}
            sub={stats.data ? stats.data.model : ''}
          />
          <Stat
            k={t('dashboard.scheduledTasks')}
            v={tasks.data ? String(tasks.data.filter((x) => x.enabled).length) : '—'}
            sub={tasks.data ? `${tasks.data.length} ${t('dashboard.total')}` : ''}
          />
        </div>

        {stats.data ? (
          <div className="grid cols-3" style={{ marginTop: 14 }}>
            <Stat k={t('dashboard.messages')} v={String(stats.data.messageCount)} />
            <Stat k={t('dashboard.conversations')} v={String(stats.data.conversationCount)} />
            <Stat
              k={t('dashboard.providers')}
              v={String(stats.data.providers.length)}
              sub={stats.data.providers.join(', ')}
            />
          </div>
        ) : null}
      </section>

      <section className="section">
        <h2 className="section-title">{t('dashboard.agentsSection')}</h2>
        <div className="card" style={{ padding: 0, overflow: 'hidden' }}>
          <table>
            <thead>
              <tr>
                <th>{t('dashboard.col.agent')}</th>
                <th>{t('dashboard.col.platform')}</th>
                <th>{t('dashboard.col.model')}</th>
                <th>{t('dashboard.col.status')}</th>
                <th>{t('dashboard.col.lastActive')}</th>
              </tr>
            </thead>
            <tbody>
              {agents.data?.map((a) => (
                <tr key={a.id}>
                  <td>
                    <strong>{a.name}</strong>
                  </td>
                  <td className="mono">{a.platform}</td>
                  <td className="mono">{a.model}</td>
                  <td>
                    <span className={`badge dot ${a.status}`}>{a.status}</span>
                  </td>
                  <td className="mono">{a.lastActive ? fmtTime(a.lastActive) : '—'}</td>
                </tr>
              ))}
            </tbody>
          </table>
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
