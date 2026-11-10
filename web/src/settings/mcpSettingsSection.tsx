import { useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { Api } from '../api/client'
import type { McpPreset, McpServerMasked, McpUpsertBody } from '../api/types'

const RESERVED_PRESET_NAMES = new Set(['exa', 'jina'])

function urlLooksCredentialed(url: string): boolean {
  const lower = url.toLowerCase()
  return (
    lower.includes('apikey=') ||
    lower.includes('api_key=') ||
    lower.includes('exaapikey=') ||
    lower.includes('token=') ||
    lower.includes('secret=') ||
    lower.includes('bearer=')
  )
}

/** Synthesize a disabled preset row when catalog has it but settings has no match. */
function resolvePresetRow(id: string, servers: McpServerMasked[]): McpServerMasked {
  const found = servers.find((s) => s.name === id || s.preset === id)
  if (found) return found
  return {
    name: id,
    kind: 'preset',
    preset: id,
    enabled: false,
    url: null,
    authConfigured: false,
    headerNames: [],
    connected: false,
    privacyAccepted: false,
    privacyAcceptedAt: null,
    configOwned: false,
  }
}

export function McpSettingsSection({
  api,
  servers,
}: {
  api: Api
  servers: McpServerMasked[]
}) {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const presetsQ = useQuery({ queryKey: ['mcp-presets'], queryFn: api.getMcpPresets })
  const [msg, setMsg] = useState<{ text: string; ok: boolean } | null>(null)

  const invalidate = () => {
    void queryClient.invalidateQueries({ queryKey: ['settings'] })
  }

  const report = (ok: boolean, text: string) => setMsg({ ok, text })

  const stdio = servers.filter((s) => s.kind === 'stdio')
  const custom = servers.filter((s) => s.kind === 'custom')
  const catalog = presetsQ.data?.presets ?? []

  return (
    <section className="section" style={{ gridColumn: '1 / -1' }}>
      <h2 className="section-title">{t('settings.mcpTitle')}</h2>
      <p className="hint" style={{ marginBottom: 12 }}>{t('settings.mcpHint')}</p>
      {presetsQ.isLoading ? (
        <div className="loading">{t('common.loading')}</div>
      ) : (
        <div className="grid cols-2" style={{ gap: 12 }}>
          {catalog.map((preset) => (
            <PresetCard
              key={preset.id}
              api={api}
              preset={preset}
              row={resolvePresetRow(preset.id, servers)}
              onDone={(ok, text) => {
                report(ok, text)
                if (ok) invalidate()
              }}
            />
          ))}
        </div>
      )}
      <CustomMcpCard
        api={api}
        servers={custom}
        onDone={(ok, text) => {
          report(ok, text)
          if (ok) invalidate()
        }}
      />
      {stdio.length > 0 ? (
        <div className="hint" style={{ marginTop: 12 }}>
          {t('settings.mcpStdioNote', { names: stdio.map((s) => s.name).join(', ') })}
        </div>
      ) : null}
      {msg ? (
        <div
          className={msg.ok ? 'hint' : 'error-note'}
          role="status"
          style={{ marginTop: 12 }}
        >
          {msg.text}
        </div>
      ) : null}
    </section>
  )
}

function PresetCard({
  api,
  preset,
  row,
  onDone,
}: {
  api: Api
  preset: McpPreset
  row: McpServerMasked
  onDone: (ok: boolean, text: string) => void
}) {
  const { t } = useTranslation()
  const isExa = preset.id === 'exa'
  const readOnly = row.configOwned
  const [apiKey, setApiKey] = useState('')
  const [privacy, setPrivacy] = useState(false)
  const [busy, setBusy] = useState(false)

  const needsPrivacy =
    isExa && !!preset.requiresPrivacyConsent && !row.privacyAccepted

  const run = async (fn: () => Promise<unknown>, okMsg: string) => {
    setBusy(true)
    try {
      await fn()
      onDone(true, okMsg)
      setApiKey('')
    } catch (e) {
      onDone(false, t('settings.mcpFailed', { message: (e as Error).message }))
    } finally {
      setBusy(false)
    }
  }

  const idPrefix = `settings-mcp-${preset.id}`

  return (
    <div className="card">
      <div className="row" style={{ justifyContent: 'space-between', marginBottom: 8 }}>
        <strong>{preset.label}</strong>
        <div className="tag-list">
          <span className={`badge ${row.enabled ? 'dot running' : ''}`}>
            {row.enabled ? t('settings.mcpEnabled') : t('settings.mcpDisabled')}
          </span>
          <span className="badge">
            {row.connected ? t('settings.mcpConnected') : t('settings.mcpNotConnected')}
          </span>
          <span className="badge">
            {row.authConfigured ? t('settings.mcpAuthConfigured') : t('settings.mcpAuthMissing')}
          </span>
        </div>
      </div>
      {preset.description ? (
        <p className="hint" style={{ marginBottom: 8 }}>{preset.description}</p>
      ) : null}
      <div className="field">
        <label htmlFor={`${idPrefix}-endpoint`}>{t('settings.mcpEndpoint')}</label>
        <input id={`${idPrefix}-endpoint`} type="text" value={preset.url} readOnly disabled />
      </div>
      <div className="field">
        <label htmlFor={`${idPrefix}-tools`}>{t('settings.mcpTools')}</label>
        <input
          id={`${idPrefix}-tools`}
          type="text"
          value={preset.tools.join(', ')}
          readOnly
          disabled
        />
      </div>
      {readOnly ? (
        <div className="banner warn" role="status" style={{ marginBottom: 8 }}>
          {t('settings.mcpConfigOwned')}
        </div>
      ) : null}
      {needsPrivacy && !readOnly ? (
        <div className="field">
          <label htmlFor={`${idPrefix}-privacy`} style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
            <input
              id={`${idPrefix}-privacy`}
              type="checkbox"
              checked={privacy}
              disabled={busy}
              onChange={(e) => setPrivacy(e.target.checked)}
            />
            {t('settings.mcpPrivacyConsent')}
          </label>
          {!privacy ? (
            <span className="hint">{t('settings.mcpPrivacyRequired')}</span>
          ) : null}
        </div>
      ) : null}
      <div className="field">
        <label htmlFor={`${idPrefix}-key`}>
          {isExa ? t('settings.mcpApiKey') : t('settings.mcpBearer')}
        </label>
        <input
          id={`${idPrefix}-key`}
          type="password"
          autoComplete="off"
          value={apiKey}
          disabled={busy || readOnly}
          placeholder={
            row.authConfigured ? '••••••••' : undefined
          }
          onChange={(e) => setApiKey(e.target.value)}
        />
        <span className="hint">
          {isExa ? t('settings.mcpApiKeyHint') : t('settings.mcpBearerHint')}
          {isExa && preset.keylessDefault ? ` ${t('settings.mcpKeylessOk')}` : ''}
        </span>
      </div>
      <div className="row" style={{ gap: 8, flexWrap: 'wrap' }}>
        {!row.enabled ? (
          <button
            type="button"
            className="primary"
            disabled={busy || readOnly || (needsPrivacy && !privacy)}
            onClick={() =>
              void run(
                () =>
                  api.enableMcpServer(
                    preset.id,
                    needsPrivacy ? { privacyAccepted: true } : undefined,
                  ),
                t('settings.mcpSaved'),
              )
            }
          >
            {t('settings.mcpEnable')}
          </button>
        ) : (
          <button
            type="button"
            className="btn-secondary"
            disabled={busy || readOnly}
            onClick={() =>
              void run(() => api.disableMcpServer(preset.id), t('settings.mcpSaved'))
            }
          >
            {t('settings.mcpDisable')}
          </button>
        )}
        <button
          type="button"
          className="btn-secondary"
          disabled={busy || readOnly || !apiKey.trim()}
          onClick={() => {
            const body: McpUpsertBody = isExa
              ? { headers: { 'x-api-key': apiKey.trim() } }
              : { bearer: apiKey.trim() }
            void run(() => api.putMcpServer(preset.id, body), t('settings.mcpSaved'))
          }}
        >
          {t('settings.mcpSaveKey')}
        </button>
        {isExa ? (
          <button
            type="button"
            className="btn-secondary"
            disabled={busy || readOnly || !row.authConfigured}
            onClick={() =>
              void run(
                () => api.putMcpServer(preset.id, { clearApiKey: true }),
                t('settings.mcpSaved'),
              )
            }
          >
            {t('settings.mcpClearApiKey')}
          </button>
        ) : null}
      </div>
      {busy ? (
        <div className="hint" role="status" style={{ marginTop: 8 }}>
          {t('settings.saving')}
        </div>
      ) : null}
    </div>
  )
}

type HeaderRow = { key: string; value: string }

function CustomMcpCard({
  api,
  servers,
  onDone,
}: {
  api: Api
  servers: McpServerMasked[]
  onDone: (ok: boolean, text: string) => void
}) {
  const { t } = useTranslation()
  const [editing, setEditing] = useState<string | null>(null)
  const [name, setName] = useState('')
  const [url, setUrl] = useState('')
  const [bearer, setBearer] = useState('')
  const [headers, setHeaders] = useState<HeaderRow[]>([])
  const [busy, setBusy] = useState(false)

  const resetForm = (row?: McpServerMasked) => {
    if (row) {
      setEditing(row.name)
      setName(row.name)
      setUrl(row.url ?? '')
      setBearer('')
      setHeaders((row.headerNames ?? []).map((k) => ({ key: k, value: '' })))
    } else {
      setEditing(null)
      setName('')
      setUrl('')
      setBearer('')
      setHeaders([])
    }
  }

  const run = async (fn: () => Promise<unknown>) => {
    setBusy(true)
    try {
      await fn()
      onDone(true, t('settings.mcpSaved'))
      resetForm()
    } catch (e) {
      onDone(false, t('settings.mcpFailed', { message: (e as Error).message }))
    } finally {
      setBusy(false)
    }
  }

  const save = () => {
    const n = name.trim()
    const u = url.trim()
    if (!n) {
      onDone(false, t('settings.mcpNameRequired'))
      return
    }
    if (RESERVED_PRESET_NAMES.has(n.toLowerCase())) {
      onDone(false, t('settings.mcpReservedName'))
      return
    }
    if (!u) {
      onDone(false, t('settings.mcpUrlRequired'))
      return
    }
    if (urlLooksCredentialed(u)) {
      onDone(false, t('settings.mcpUrlCredential'))
      return
    }
    const headerMap: Record<string, string> = {}
    for (const h of headers) {
      const k = h.key.trim()
      const v = h.value.trim()
      if (k && v) headerMap[k] = v
    }
    const body: McpUpsertBody = {
      name: n,
      url: u,
      ...(bearer.trim() ? { bearer: bearer.trim() } : {}),
      ...(Object.keys(headerMap).length ? { headers: headerMap } : {}),
    }
    const target = editing ?? n
    void run(() => api.putMcpServer(target, body))
  }

  return (
    <div className="card" style={{ marginTop: 12 }}>
      <h3 style={{ margin: '0 0 8px', fontSize: 15 }}>{t('settings.mcpCustomTitle')}</h3>
      <p className="hint" style={{ marginBottom: 12 }}>{t('settings.mcpCustomHint')}</p>
      {servers.length === 0 ? (
        <div className="hint">{t('settings.mcpNoCustom')}</div>
      ) : (
        <ul style={{ listStyle: 'none', padding: 0, margin: '0 0 12px' }}>
          {servers.map((s) => (
            <li
              key={s.name}
              className="row"
              style={{ justifyContent: 'space-between', padding: '6px 0', gap: 8, flexWrap: 'wrap' }}
            >
              <span>
                <strong className="mono">{s.name}</strong>{' '}
                <span className="hint">{s.url}</span>{' '}
                <span className="badge">
                  {s.enabled ? t('settings.mcpEnabled') : t('settings.mcpDisabled')}
                </span>{' '}
                <span className="badge">
                  {s.connected ? t('settings.mcpConnected') : t('settings.mcpNotConnected')}
                </span>
              </span>
              <span className="row" style={{ gap: 6 }}>
                <button
                  type="button"
                  className="sm"
                  disabled={busy}
                  onClick={() => resetForm(s)}
                >
                  {t('settings.mcpEditCustom')}
                </button>
                {s.enabled ? (
                  <button
                    type="button"
                    className="sm"
                    disabled={busy}
                    onClick={() => void run(() => api.disableMcpServer(s.name))}
                  >
                    {t('settings.mcpDisable')}
                  </button>
                ) : (
                  <button
                    type="button"
                    className="sm primary"
                    disabled={busy}
                    onClick={() => void run(() => api.enableMcpServer(s.name))}
                  >
                    {t('settings.mcpEnable')}
                  </button>
                )}
                <button
                  type="button"
                  className="sm"
                  disabled={busy}
                  onClick={() => {
                    if (!window.confirm(t('settings.mcpDeleteConfirm', { name: s.name }))) return
                    void run(() => api.deleteMcpServer(s.name))
                  }}
                >
                  {t('settings.mcpDelete')}
                </button>
              </span>
            </li>
          ))}
        </ul>
      )}

      <div className="row" style={{ marginBottom: 8 }}>
        <strong>{editing ? t('settings.mcpEditCustom') : t('settings.mcpNewCustom')}</strong>
        {editing ? (
          <button type="button" className="sm" disabled={busy} onClick={() => resetForm()}>
            {t('common.cancel')}
          </button>
        ) : null}
      </div>
      <div className="field">
        <label htmlFor="settings-mcp-custom-name">{t('settings.mcpName')}</label>
        <input
          id="settings-mcp-custom-name"
          type="text"
          value={name}
          disabled={busy || !!editing}
          onChange={(e) => setName(e.target.value)}
        />
      </div>
      <div className="field">
        <label htmlFor="settings-mcp-custom-url">{t('settings.mcpUrl')}</label>
        <input
          id="settings-mcp-custom-url"
          type="url"
          value={url}
          disabled={busy}
          onChange={(e) => setUrl(e.target.value)}
        />
      </div>
      <div className="field">
        <label htmlFor="settings-mcp-custom-bearer">{t('settings.mcpBearer')}</label>
        <input
          id="settings-mcp-custom-bearer"
          type="password"
          autoComplete="off"
          value={bearer}
          disabled={busy}
          onChange={(e) => setBearer(e.target.value)}
        />
        <span className="hint">{t('settings.mcpBearerHint')}</span>
      </div>
      <fieldset className="field">
        <legend>{t('settings.mcpHeaders')}</legend>
        {headers.map((h, i) => (
          <div key={i} className="row" style={{ gap: 8, marginBottom: 6, flexWrap: 'wrap' }}>
            <input
              type="text"
              aria-label={t('settings.mcpHeaderName')}
              placeholder={t('settings.mcpHeaderName')}
              value={h.key}
              disabled={busy}
              onChange={(e) => {
                const next = [...headers]
                next[i] = { ...next[i], key: e.target.value }
                setHeaders(next)
              }}
              style={{ flex: 1, minWidth: 120 }}
            />
            <input
              type="password"
              autoComplete="off"
              aria-label={t('settings.mcpHeaderValue')}
              placeholder={t('settings.mcpHeaderValue')}
              value={h.value}
              disabled={busy}
              onChange={(e) => {
                const next = [...headers]
                next[i] = { ...next[i], value: e.target.value }
                setHeaders(next)
              }}
              style={{ flex: 1, minWidth: 120 }}
            />
            <button
              type="button"
              className="sm"
              disabled={busy}
              onClick={() => setHeaders(headers.filter((_, j) => j !== i))}
            >
              {t('settings.mcpRemoveHeader')}
            </button>
          </div>
        ))}
        <div className="row" style={{ gap: 8, flexWrap: 'wrap' }}>
          <button
            type="button"
            className="sm"
            disabled={busy}
            onClick={() => setHeaders([...headers, { key: '', value: '' }])}
          >
            {t('settings.mcpAddHeader')}
          </button>
          {editing &&
          (servers.find((s) => s.name === editing)?.headerNames?.length ?? 0) > 0 ? (
            <button
              type="button"
              className="sm"
              disabled={busy}
              onClick={() =>
                void run(() => api.putMcpServer(editing, { headers: {} }))
              }
            >
              {t('settings.mcpClearHeaders')}
            </button>
          ) : null}
        </div>
      </fieldset>
      <div className="row" style={{ gap: 8 }}>
        <button type="button" className="primary" disabled={busy} onClick={save}>
          {busy ? t('settings.saving') : t('settings.mcpSave')}
        </button>
      </div>
      {busy ? (
        <div className="hint" role="status" style={{ marginTop: 8 }}>
          {t('settings.saving')}
        </div>
      ) : null}
    </div>
  )
}
