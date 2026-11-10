import { useQueryClient } from '@tanstack/react-query'
import { useMemo, useState, type ReactNode } from 'react'
import { useTranslation } from 'react-i18next'
import type { Api } from '../api/client'
import type { McpRegistryListEntry } from '../mcp/registryTypes'
import {
  REGISTRY_SEARCH_MIN_CHARS,
  classifyRegistryServer,
  collectRegistrySecretFields,
  filterRegistryEntriesRemoteOnly,
  installRegistryServersSequential,
  mapRegistryRemoteToUpsert,
  pickInstallableRemote,
  registrySearchQueryValid,
  slugFromRegistryName,
} from '../mcp/registryUtils'

type SelectedDraft = {
  registryName: string
  slug: string
  entry: McpRegistryListEntry
  secrets: Record<string, string>
}

export function McpRegistrySection({
  api,
  existingServerNames,
}: {
  api: Api
  existingServerNames: string[]
}) {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const [query, setQuery] = useState('')
  const [remoteOnly, setRemoteOnly] = useState(true)
  const [entries, setEntries] = useState<McpRegistryListEntry[]>([])
  const [nextCursor, setNextCursor] = useState<string | null>(null)
  const [selected, setSelected] = useState<Record<string, SelectedDraft>>({})
  const [searching, setSearching] = useState(false)
  const [loadingMore, setLoadingMore] = useState(false)
  const [installing, setInstalling] = useState(false)
  const [registryDown, setRegistryDown] = useState(false)
  const [status, setStatus] = useState<{ ok: boolean; text: string } | null>(null)

  const takenNames = useMemo(() => {
    const s = new Set(existingServerNames.map((n) => n.toLowerCase()))
    for (const d of Object.values(selected)) s.add(d.slug.toLowerCase())
    return s
  }, [existingServerNames, selected])

  const visibleEntries = useMemo(() => {
    return remoteOnly ? filterRegistryEntriesRemoteOnly(entries) : entries
  }, [entries, remoteOnly])

  const runSearch = async (cursor?: string) => {
    const q = query.trim()
    if (!registrySearchQueryValid(q)) {
      setStatus({ ok: false, text: t('mcpRegistry.searchTooShort', { min: REGISTRY_SEARCH_MIN_CHARS }) })
      return
    }
    setStatus(null)
    setRegistryDown(false)
    const setLoading = cursor ? setLoadingMore : setSearching
    setLoading(true)
    try {
      const res = await api.searchMcpRegistry({
        q: q || undefined,
        cursor,
        limit: 25,
      })
      setRegistryDown(false)
      setNextCursor(res.metadata.nextCursor ?? null)
      if (cursor) {
        setEntries((prev) => [...prev, ...res.servers])
      } else {
        setEntries(res.servers)
      }
      if (res.servers.length === 0 && !cursor) {
        setStatus({ ok: true, text: t('mcpRegistry.noResults') })
      }
    } catch (e) {
      setRegistryDown(true)
      setStatus({
        ok: false,
        text: t('mcpRegistry.registryUnavailable', { message: (e as Error).message }),
      })
      if (!cursor) setEntries([])
    } finally {
      setLoading(false)
    }
  }

  const toggleSelect = (entry: McpRegistryListEntry) => {
    const name = entry.server.name
    const kind = classifyRegistryServer(entry.server)
    if (kind !== 'installable') return
    setSelected((prev) => {
      if (prev[name]) {
        const next = { ...prev }
        delete next[name]
        return next
      }
      const slug = slugFromRegistryName(name, takenNames)
      return {
        ...prev,
        [name]: { registryName: name, slug, entry, secrets: {} },
      }
    })
  }

  const updateSlug = (registryName: string, slug: string) => {
    setSelected((prev) => {
      const row = prev[registryName]
      if (!row) return prev
      return { ...prev, [registryName]: { ...row, slug } }
    })
  }

  const updateSecret = (registryName: string, headerName: string, value: string) => {
    setSelected((prev) => {
      const row = prev[registryName]
      if (!row) return prev
      return {
        ...prev,
        [registryName]: {
          ...row,
          secrets: { ...row.secrets, [headerName]: value },
        },
      }
    })
  }

  const installSelected = async () => {
    const drafts = Object.values(selected)
    if (drafts.length === 0) return
    setInstalling(true)
    setStatus(null)
    const payloads: { slug: string; body: ReturnType<typeof mapRegistryRemoteToUpsert> }[] = []
    for (const d of drafts) {
      const remote = pickInstallableRemote(d.entry.server)
      if (!remote) continue
      const slug = d.slug.trim()
      if (!slug) {
        setStatus({ ok: false, text: t('mcpRegistry.slugRequired') })
        setInstalling(false)
        return
      }
      payloads.push({
        slug,
        body: mapRegistryRemoteToUpsert(remote, slug, d.secrets),
      })
    }
    const results = await installRegistryServersSequential(payloads, api.putMcpServer.bind(api))
    const ok = results.filter((r) => r.ok).length
    const fail = results.filter((r) => !r.ok)
    void queryClient.invalidateQueries({ queryKey: ['settings'] })
    if (fail.length === 0) {
      setSelected({})
      setStatus({ ok: true, text: t('mcpRegistry.installAllOk', { count: ok }) })
    } else {
      const detail = fail.map((f) => `${f.slug}: ${f.error ?? 'error'}`).join('; ')
      setStatus({
        ok: false,
        text: t('mcpRegistry.installPartial', { ok, fail: fail.length, detail }),
      })
    }
    setInstalling(false)
  }

  const queryInvalid = query.trim().length > 0 && !registrySearchQueryValid(query)

  return (
    <section className="section" style={{ gridColumn: '1 / -1', marginTop: 16 }}>
      <h2 className="section-title">{t('mcpRegistry.title')}</h2>
      <p className="hint" style={{ marginBottom: 12 }}>
        {t('mcpRegistry.hint')}
      </p>
      {registryDown ? (
        <div className="banner warn" role="status" style={{ marginBottom: 12 }}>
          {t('mcpRegistry.registryDownBanner')}
        </div>
      ) : null}
      <div className="row" style={{ gap: 8, flexWrap: 'wrap', marginBottom: 8 }}>
        <RegistrySearchField
          id="mcp-registry-search"
          label={t('mcpRegistry.searchLabel')}
          hint={
            queryInvalid
              ? t('mcpRegistry.searchTooShort', { min: REGISTRY_SEARCH_MIN_CHARS })
              : t('mcpRegistry.searchHint', { min: REGISTRY_SEARCH_MIN_CHARS })
          }
        >
          <input
            id="mcp-registry-search"
            type="search"
            value={query}
            disabled={searching || installing}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter') void runSearch()
            }}
            placeholder={t('mcpRegistry.searchPlaceholder')}
            style={{ minWidth: 220, flex: 1 }}
          />
        </RegistrySearchField>
        <button
          type="button"
          className="primary"
          disabled={searching || installing || queryInvalid}
          onClick={() => void runSearch()}
        >
          {searching ? t('mcpRegistry.searching') : t('mcpRegistry.search')}
        </button>
        <label className="row" style={{ gap: 6, alignItems: 'center' }}>
          <input
            type="checkbox"
            checked={remoteOnly}
            disabled={searching || installing}
            onChange={(e) => setRemoteOnly(e.target.checked)}
          />
          {t('mcpRegistry.remoteOnly')}
        </label>
      </div>

      {visibleEntries.length > 0 ? (
        <ul style={{ listStyle: 'none', padding: 0, margin: '0 0 12px' }}>
          {visibleEntries.map((entry) => (
            <RegistryResultRow
              key={entry.server.name}
              entry={entry}
              checked={!!selected[entry.server.name]}
              disabled={installing}
              onToggle={() => toggleSelect(entry)}
            />
          ))}
        </ul>
      ) : null}

      {nextCursor ? (
        <button
          type="button"
          className="btn-secondary sm"
          disabled={loadingMore || searching || installing}
          onClick={() => void runSearch(nextCursor)}
        >
          {loadingMore ? t('common.loading') : t('mcpRegistry.loadMore')}
        </button>
      ) : null}

      {Object.keys(selected).length > 0 ? (
        <div className="card" style={{ marginTop: 12 }}>
          <h3 style={{ margin: '0 0 8px', fontSize: 15 }}>{t('mcpRegistry.reviewTitle')}</h3>
          <p className="hint" style={{ marginBottom: 8 }}>
            {t('mcpRegistry.reviewHint')}
          </p>
          <ul style={{ listStyle: 'none', padding: 0, margin: 0 }}>
            {Object.values(selected).map((d) => {
              const remote = pickInstallableRemote(d.entry.server)
              const secretFields = remote ? collectRegistrySecretFields(remote) : []
              return (
                <li key={d.registryName} style={{ padding: '8px 0', borderTop: '1px solid var(--border)' }}>
                  <div className="hint" style={{ marginBottom: 4 }}>
                    {d.registryName}
                  </div>
                  <div className="field">
                    <label htmlFor={`slug-${d.registryName}`}>{t('mcpRegistry.slug')}</label>
                    <input
                      id={`slug-${d.registryName}`}
                      type="text"
                      className="mono"
                      value={d.slug}
                      disabled={installing}
                      onChange={(e) => updateSlug(d.registryName, e.target.value)}
                    />
                  </div>
                  {remote ? (
                    <div className="hint" style={{ marginBottom: 6 }}>
                      {remote.url}
                    </div>
                  ) : null}
                  {secretFields.map((f) => (
                    <div className="field" key={f.name}>
                      <label htmlFor={`secret-${d.registryName}-${f.name}`}>
                        {f.isBearer ? t('mcpRegistry.bearerFor', { name: f.name }) : f.name}
                      </label>
                      <input
                        id={`secret-${d.registryName}-${f.name}`}
                        type="password"
                        autoComplete="off"
                        disabled={installing}
                        value={d.secrets[f.name] ?? ''}
                        onChange={(e) => updateSecret(d.registryName, f.name, e.target.value)}
                      />
                      {f.description ? (
                        <span className="hint">{f.description}</span>
                      ) : null}
                    </div>
                  ))}
                </li>
              )
            })}
          </ul>
          <button
            type="button"
            className="primary"
            disabled={installing}
            onClick={() => void installSelected()}
          >
            {installing ? t('mcpRegistry.installing') : t('mcpRegistry.installSelected')}
          </button>
        </div>
      ) : null}

      {status ? (
        <div
          className={status.ok ? 'hint' : 'error-note'}
          role="status"
          style={{ marginTop: 12 }}
        >
          {status.text}
        </div>
      ) : null}
    </section>
  )
}

function RegistrySearchField({
  id,
  label,
  hint,
  children,
}: {
  id: string
  label: string
  hint?: string
  children: ReactNode
}) {
  return (
    <div className="field" style={{ flex: 1, minWidth: 200 }}>
      <label htmlFor={id}>{label}</label>
      {children}
      {hint ? <span className="hint">{hint}</span> : null}
    </div>
  )
}

function RegistryResultRow({
  entry,
  checked,
  disabled,
  onToggle,
}: {
  entry: McpRegistryListEntry
  checked: boolean
  disabled: boolean
  onToggle: () => void
}) {
  const { t } = useTranslation()
  const kind = classifyRegistryServer(entry.server)
  const official = entry._meta?.['io.modelcontextprotocol.registry/official']
  const installable = kind === 'installable'

  return (
    <li
      className="row"
      style={{
        justifyContent: 'space-between',
        alignItems: 'flex-start',
        padding: '8px 0',
        gap: 8,
        flexWrap: 'wrap',
        borderTop: '1px solid var(--border)',
      }}
    >
      <div style={{ flex: 1, minWidth: 200 }}>
        <div className="row" style={{ gap: 8, flexWrap: 'wrap', alignItems: 'center' }}>
          {installable ? (
            <input
              type="checkbox"
              aria-label={t('mcpRegistry.selectServer', { name: entry.server.name })}
              checked={checked}
              disabled={disabled}
              onChange={onToggle}
            />
          ) : (
            <span style={{ width: 18 }} aria-hidden />
          )}
          <strong>{entry.server.title ?? entry.server.name.split('/').pop()}</strong>
          {official?.status ? (
            <span className="badge">{official.status}</span>
          ) : null}
          {kind === 'stdio' ? (
            <span className="badge">{t('mcpRegistry.badgeStdio')}</span>
          ) : null}
          {kind === 'sse-only' ? (
            <span className="badge">{t('mcpRegistry.badgeSse')}</span>
          ) : null}
        </div>
        <div className="hint mono" style={{ marginTop: 2 }}>
          {entry.server.name}
        </div>
        {entry.server.description ? (
          <p className="hint" style={{ margin: '4px 0 0' }}>
            {entry.server.description}
          </p>
        ) : null}
        {kind === 'stdio' ? (
          <p className="hint">{t('mcpRegistry.stdioHint')}</p>
        ) : null}
      </div>
    </li>
  )
}
