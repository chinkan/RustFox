import { createFileRoute, useNavigate } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { z } from 'zod'
import type { MemoryKind } from '../../api/types'

const memorySearchSchema = z.object({
  q: z.string().default('').catch(''),
  kind: z.enum(['all', 'fact', 'knowledge', 'conversation']).default('all').catch('all'),
})

export const Route = createFileRoute('/_auth/memory')({
  validateSearch: memorySearchSchema,
  component: MemoryPage,
})

const KINDS = ['all', 'fact', 'knowledge', 'conversation'] as const

function MemoryPage() {
  const { api } = Route.useRouteContext()
  const { q, kind } = Route.useSearch()
  const navigate = useNavigate({ from: Route.fullPath })
  const { t } = useTranslation()

  const results = useQuery({
    queryKey: ['memory', q, kind],
    queryFn: () => api.searchMemory(q, kind === 'all' ? undefined : (kind as MemoryKind)),
  })

  return (
    <>
      <div className="page-head">
        <h1>{t('memory.title')}</h1>
        <p>{t('memory.subtitle')}</p>
      </div>

      <div className="row" style={{ marginBottom: 18 }}>
        <input
          type="search"
          placeholder={t('memory.searchPlaceholder')}
          value={q}
          onChange={(e) => void navigate({ search: (prev) => ({ ...prev, q: e.target.value }) })}
          style={{ width: 300 }}
        />
        <div className="tag-list">
          {KINDS.map((k) => (
            <button
              key={k}
              className={kind === k ? 'sm primary' : 'sm'}
              onClick={() => void navigate({ search: (prev) => ({ ...prev, kind: k }) })}
            >
              {t(`memory.${k}`)}
            </button>
          ))}
        </div>
      </div>

      {results.isLoading ? (
        <div className="loading">{t('common.loading')}</div>
      ) : results.isError ? (
        <div className="error-note">{(results.error as Error).message}</div>
      ) : (
        <div className="grid">
          {results.data?.map((m) => (
            <div key={m.id} className="card">
              <div className="row">
                <span className="badge">{t(`memory.${m.kind}`)}</span>
                <span className="spacer" />
                <span className="mono">score {m.score.toFixed(2)}</span>
              </div>
              <p style={{ margin: '9px 0 0', fontSize: 13.5 }}>{m.text}</p>
            </div>
          ))}
          {results.data?.length === 0 ? (
            <div className="empty">
              <div className="big">🧠</div>
              <p>{t('memory.noneFound', { q })}</p>
            </div>
          ) : null}
        </div>
      )}
    </>
  )
}
