import { createFileRoute } from '@tanstack/react-router'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { z } from 'zod'
import { PixelOffice, SeatStatus } from '../../agents/PixelOffice'
import { useOfficeMotion } from '../../agents/officePrefs'
import { agentsPollInterval } from '../../agents/seats'

/**
 * Type-safe search params — the TanStack Router killer feature.
 * Every filter lives in the URL and is validated + inferred by zod.
 */
const agentSearchSchema = z.object({
  q: z.string().default('').catch(''),
  status: z.enum(['all', 'idle', 'running', 'error']).default('all').catch('all'),
  sort: z.enum(['name', 'status']).default('name').catch('name'),
  view: z.enum(['office', 'table']).default('office').catch('office'),
})

export const Route = createFileRoute('/_auth/agents')({
  validateSearch: agentSearchSchema,
  component: AgentsPage,
})

function AgentsPage() {
  const { api } = Route.useRouteContext()
  const { q, status, sort, view } = Route.useSearch()
  const navigate = Route.useNavigate()
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const [reloadMsg, setReloadMsg] = useState<string | null>(null)
  const motion = useOfficeMotion()

  // Poll only while this page is mounted. refetchIntervalInBackground pauses
  // the timer when the document is hidden (focusManager treats hidden as
  // unfocused). The interval function also returns false while hidden.
  const agents = useQuery({
    queryKey: ['agents'],
    queryFn: api.listAgents,
    refetchInterval: () =>
      agentsPollInterval(typeof document !== 'undefined' && document.visibilityState !== 'hidden'),
    refetchIntervalInBackground: false,
  })
  const skills = useQuery({ queryKey: ['skills'], queryFn: api.listSkills })

  const reload = useMutation({
    mutationFn: api.reloadAgents,
    onSuccess: (r) => {
      setReloadMsg(t('agents.reloadDone', { skills: r.skillsLoaded, agents: r.agentsLoaded }))
      void queryClient.invalidateQueries({ queryKey: ['agents'] })
      void queryClient.invalidateQueries({ queryKey: ['skills'] })
    },
  })

  const filtered = (agents.data ?? [])
    .filter((a) => (status === 'all' ? true : a.status === status))
    .filter((a) => a.name.toLowerCase().includes(q.toLowerCase()))
    .sort((a, b) =>
      sort === 'name' ? a.name.localeCompare(b.name) : a.status.localeCompare(b.status),
    )

  return (
    <>
      <div className="page-head">
        <h1>{t('agents.title')}</h1>
        <p>{t('agents.subtitle')}</p>
      </div>

      <div className="row" style={{ marginBottom: 18 }}>
        <div className="segmented" role="group" aria-label={t('agents.viewLabel')}>
          <button
            type="button"
            className={view === 'office' ? 'sm primary' : 'sm ghost'}
            aria-pressed={view === 'office'}
            onClick={() => void navigate({ search: (prev) => ({ ...prev, view: 'office' }) })}
          >
            {t('agents.viewOffice')}
          </button>
          <button
            type="button"
            className={view === 'table' ? 'sm primary' : 'sm ghost'}
            aria-pressed={view === 'table'}
            onClick={() => void navigate({ search: (prev) => ({ ...prev, view: 'table' }) })}
          >
            {t('agents.viewTable')}
          </button>
        </div>

        <input
          type="search"
          placeholder={t('agents.searchPlaceholder')}
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
          <option value="all">{t('agents.allStatuses')}</option>
          <option value="running">{t('agents.running')}</option>
          <option value="idle">{t('agents.idle')}</option>
          <option value="error">{t('agents.error')}</option>
        </select>

        <select
          value={sort}
          onChange={(e) =>
            void navigate({
              search: (prev) => ({ ...prev, sort: e.target.value as typeof sort }),
            })
          }
        >
          <option value="name">{t('agents.sortName')}</option>
          <option value="status">{t('agents.sortStatus')}</option>
        </select>

        <span className="spacer" />
        <button
          className="ghost sm"
          onClick={() => reload.mutate()}
          disabled={reload.isPending}
        >
          {t('agents.reload')}
        </button>
        <span className="badge">{t('agents.shown', { count: filtered.length })}</span>
      </div>

      {reloadMsg ? <div className="hint" style={{ marginBottom: 12 }}>{reloadMsg}</div> : null}

      {view === 'office' ? (
        <PixelOffice
          agents={filtered}
          motion={motion}
          onShowOverflow={() => void navigate({ search: (prev) => ({ ...prev, view: 'table' }) })}
        />
      ) : (
        <div className="card" style={{ padding: 0, overflow: 'hidden' }}>
          <table>
            <thead>
              <tr>
                <th>{t('agents.col.agent')}</th>
                <th>{t('agents.col.platform')}</th>
                <th>{t('agents.col.model')}</th>
                <th>{t('agents.col.status')}</th>
              </tr>
            </thead>
            <tbody>
              {filtered.map((a) => (
                <tr key={a.id}>
                  <td>
                    <strong>{a.name}</strong>
                  </td>
                  <td className="mono">{a.platform}</td>
                  <td className="mono">{a.model}</td>
                  <td>
                    <SeatStatus agent={a} />
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
          {filtered.length === 0 ? (
            <div className="empty">
              <div className="big">🔍</div>
              <p>{t('agents.noneFound')}</p>
            </div>
          ) : null}
        </div>
      )}

      <section className="section">
        <h2 className="section-title">Skills ({skills.data?.length ?? '…'})</h2>
        <div className="grid cols-2">
          {skills.data?.map((s) => (
            <div key={s.name} className="card">
              <div className="row">
                <strong style={{ fontSize: 13.5 }}>{s.name}</strong>
                <span className="spacer" />
                <span className="badge">{s.kind}</span>
              </div>
              <p style={{ color: 'var(--text-dim)', fontSize: 12.5, margin: '6px 0 0' }}>
                {s.description}
              </p>
            </div>
          ))}
        </div>
      </section>
    </>
  )
}
