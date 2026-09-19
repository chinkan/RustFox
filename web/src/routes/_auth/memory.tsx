import { createFileRoute, useNavigate } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'
import { z } from 'zod'
import type { MemoryEntry } from '../../api/types'

const memorySearchSchema = z.object({
  q: z.string().default('').catch(''),
  kind: z.enum(['all', 'fact', 'knowledge', 'conversation']).default('all').catch('all'),
})

export const Route = createFileRoute('/_auth/memory')({
  validateSearch: memorySearchSchema,
  component: MemoryPage,
})

const KIND_LABEL: Record<MemoryEntry['kind'], string> = {
  fact: 'Fact',
  knowledge: 'Knowledge',
  conversation: 'Conversation',
}

function MemoryPage() {
  const { api } = Route.useRouteContext()
  const { q, kind } = Route.useSearch()
  const navigate = useNavigate({ from: Route.fullPath })

  const results = useQuery({
    queryKey: ['memory', q, kind],
    queryFn: () => api.searchMemory(q, kind === 'all' ? undefined : kind),
  })

  return (
    <>
      <div className="page-head">
        <h1>Memory</h1>
        <p>
          Long-term memory browser — semantic + full-text search. Query is
          URL-driven and type-safe.
        </p>
      </div>

      <div className="row" style={{ marginBottom: 18 }}>
        <input
          type="search"
          placeholder="Search memory…"
          value={q}
          onChange={(e) => void navigate({ search: (prev) => ({ ...prev, q: e.target.value }) })}
          style={{ width: 300 }}
        />
        <div className="tag-list">
          {(['all', 'fact', 'knowledge', 'conversation'] as const).map((k) => (
            <button
              key={k}
              className={kind === k ? 'sm primary' : 'sm'}
              onClick={() => void navigate({ search: (prev) => ({ ...prev, kind: k }) })}
            >
              {k === 'all' ? 'All' : KIND_LABEL[k]}
            </button>
          ))}
        </div>
      </div>

      {results.isLoading ? (
        <div className="loading">Searching…</div>
      ) : (
        <div className="grid">
          {results.data?.map((m) => (
            <div key={m.id} className="card">
              <div className="row">
                <span className="badge">{KIND_LABEL[m.kind]}</span>
                <span className="spacer" />
                <span className="mono">score {m.score.toFixed(2)}</span>
              </div>
              <p style={{ margin: '9px 0 0', fontSize: 13.5 }}>{m.text}</p>
            </div>
          ))}
          {results.data?.length === 0 ? (
            <div className="empty">
              <div className="big">🧠</div>
              <p>Nothing matched “{q}”.</p>
            </div>
          ) : null}
        </div>
      )}
    </>
  )
}
