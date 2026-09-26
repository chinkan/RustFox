import { createFileRoute, redirect } from '@tanstack/react-router'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { AutonomyMode, SettingsPatch, SoulName } from '../../api/types'
import { ensureAuthBootstrapped } from '../../auth'

/**
 * `/settings` — admin-only (RBAC at the route level). Talks to the real
 * whitelisted settings API (ADR 0006): editable scalars via PATCH, masked
 * secrets read-only, and a soul-file editor with server-side `.bak` backup.
 */
export const Route = createFileRoute('/_auth/settings')({
  beforeLoad: async ({ context }) => {
    await ensureAuthBootstrapped()
    if (!context.auth.isAdmin) {
      throw redirect({ to: '/dashboard' })
    }
  },
  component: SettingsPage,
})

const SOUL_FILES: SoulName[] = ['SOUL.md', 'USER.md', 'AGENTS.md', 'system']

/** Label for the soul buttons; 'system' reads as the prompt file, not a .md. */
function soulLabel(f: SoulName): string {
  return f === 'system' ? 'prompts/system.md' : f
}

function SettingsPage() {
  const { api, auth } = Route.useRouteContext()
  const { t } = useTranslation()
  const queryClient = useQueryClient()

  const settings = useQuery({ queryKey: ['settings'], queryFn: api.getSettings })

  const [form, setForm] = useState<SettingsPatch>({
    model: '', generalLocation: '', defaultAutonomyMode: 'default', portalPort: 8090,
  })
  const [saved, setSaved] = useState(false)
  const [pendingRestart, setPendingRestart] = useState<string[]>([])
  const [soulFile, setSoulFile] = useState<SoulName | null>(null)
  const [soulText, setSoulText] = useState('')
  const [soulMsg, setSoulMsg] = useState<string | null>(null)

  // Seed the form once data arrives (and refresh after invalidation).
  useEffect(() => {
    if (settings.data) {
      const e = settings.data.editable
      setForm({
        model: e.model,
        generalLocation: e.generalLocation,
        defaultAutonomyMode: (['default', 'autopilot', 'plan'].includes(e.defaultAutonomyMode)
          ? e.defaultAutonomyMode
          : 'default') as AutonomyMode,
        portalPort: e.portalPort,
      })
    }
  }, [settings.data])

  const soul = useQuery({
    queryKey: ['soul', soulFile],
    queryFn: () => api.getSoul(soulFile as SoulName),
    enabled: soulFile !== null,
  })
  useEffect(() => {
    if (soul.data) setSoulText(soul.data.content)
  }, [soul.data])

  const patch = useMutation({
    mutationFn: (p: SettingsPatch) => api.patchSettings(p),
    onSuccess: (res) => {
      setSaved(true)
      setPendingRestart(res.restartRequired)
      setTimeout(() => setSaved(false), 4000)
      void queryClient.invalidateQueries({ queryKey: ['settings'] })
      void queryClient.invalidateQueries({ queryKey: ['stats'] })
    },
  })

  const putSoul = useMutation({
    mutationFn: () => api.putSoul(soulFile as SoulName, soulText),
    onSuccess: (r) => {
      setSoulMsg(t('settings.soulSaved', { name: r.name, bytes: r.bytes }))
      void queryClient.invalidateQueries({ queryKey: ['soul', r.name] })
      // A prompt-file save can flip the live source (builtin/inline → file),
      // so refresh the provenance card too.
      void queryClient.invalidateQueries({ queryKey: ['settings'] })
    },
    onError: (e: Error) => setSoulMsg(e.message),
  })

  const editable = settings.data?.editable
  const masked = settings.data?.masked
  const promptInfo = settings.data?.systemPrompt
  const dirty =
    !!editable &&
    ((form.model ?? '') !== editable.model ||
      (form.generalLocation ?? '') !== editable.generalLocation ||
      (form.defaultAutonomyMode ?? '') !== editable.defaultAutonomyMode ||
      (form.portalPort ?? editable.portalPort) !== editable.portalPort)

  return (
    <>
      <div className="page-head">
        <h1>{t('settings.title')}</h1>
        <p>{t('settings.subtitle')}</p>
      </div>

      {pendingRestart.length > 0 ? (
        <div className="banner warn" role="status" style={{ marginBottom: 16 }}>
          {t('settings.restartBanner', { fields: pendingRestart.join(', ') })}
        </div>
      ) : null}

      {settings.isLoading ? (
        <div className="loading">{t('common.loading')}</div>
      ) : settings.isError ? (
        <div className="error-note">{(settings.error as Error).message}</div>
      ) : editable ? (
        <div className="grid cols-2">
          <section className="section">
            <h2 className="section-title">{t('settings.editable')}</h2>
            <div className="card">
              <div className="field">
                <label>{t('settings.model')}</label>
                <input
                  type="text"
                  value={form.model ?? editable.model}
                  onChange={(e) => setForm({ ...form, model: e.target.value })}
                />
              </div>
              <div className="field">
                <label>{t('settings.generalLocation')}</label>
                <input
                  type="text"
                  value={form.generalLocation ?? editable.generalLocation}
                  onChange={(e) => setForm({ ...form, generalLocation: e.target.value })}
                />
              </div>
              <div className="field">
                <label>{t('settings.defaultAutonomyMode')}</label>
                <select
                  value={form.defaultAutonomyMode ?? editable.defaultAutonomyMode}
                  onChange={(e) =>
                    setForm({ ...form, defaultAutonomyMode: e.target.value as AutonomyMode })
                  }
                >
                  <option value="default">default</option>
                  <option value="autopilot">autopilot</option>
                  <option value="plan">plan</option>
                </select>
              </div>
              <div className="field">
                <label>{t('settings.portalPort')}</label>
                <input
                  type="number"
                  min={1}
                  max={65535}
                  value={form.portalPort ?? editable.portalPort}
                  onChange={(e) => setForm({ ...form, portalPort: Number(e.target.value) })}
                />
                <span className="hint">{settings.data?.restartRequired.includes('portalPort') ? 'restart required' : ''}</span>
              </div>
              <div className="row">
                <button
                  className="primary"
                  disabled={!dirty || patch.isPending}
                  onClick={() => patch.mutate(form as SettingsPatch)}
                >
                  {patch.isPending ? t('settings.saving') : t('settings.save')}
                </button>
                {saved ? <span className="badge dot running">{t('settings.saved')}</span> : null}
                {patch.isError ? (
                  <span className="error-note">
                    {t('settings.saveFailed', { message: (patch.error as Error).message })}
                  </span>
                ) : null}
              </div>
            </div>
          </section>

          <section className="section">
            <h2 className="section-title">{t('settings.maskedTitle')}</h2>
            <div className="card">
              <SecretRow k={t('settings.telegramBotToken')} v={masked?.telegramBotToken ?? ''} />
              <SecretRow k={t('settings.openrouterApiKey')} v={masked?.openrouterApiKey ?? ''} />
              {masked?.embeddingApiKey ? (
                <SecretRow k="Embedding API key" v={masked.embeddingApiKey} />
              ) : null}
              <div className="field" style={{ marginTop: 10 }}>
                <label>{t('settings.providers')}</label>
                <table>
                  <tbody>
                    {masked?.providers.map((p) => (
                      <tr key={p.name}>
                        <td className="mono">{p.name}</td>
                        <td className="mono">{p.model}</td>
                        <td className="mono">{p.apiKeyMasked ?? '—'}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
              {masked && masked.mcpServers.length > 0 ? (
                <div className="tag-list" style={{ marginTop: 8 }}>
                  {masked.mcpServers.map((m) => (
                    <span key={m.name} className="badge">{m.name}</span>
                  ))}
                </div>
              ) : null}
            </div>
          </section>

          <section className="section" style={{ gridColumn: '1 / -1' }}>
            <h2 className="section-title">{t('settings.promptTitle')}</h2>
            {promptInfo ? (
              <div className="card">
                <div className="hint" role="status" style={{ marginBottom: 6 }}>
                  {promptInfo.source === 'file'
                    ? t('settings.promptSourceFile', { pointer: promptInfo.pointer ?? '' })
                    : promptInfo.source === 'inline'
                      ? t('settings.promptSourceInline')
                      : t('settings.promptSourceBuiltin')}
                </div>
                {promptInfo.divergence ? (
                  <div className="banner warn" role="alert">
                    {t('settings.promptDivergence')}
                  </div>
                ) : null}
                {promptInfo.pointer === null ? (
                  <div className="hint" style={{ marginTop: 4 }}>
                    {t('settings.promptNotEnabled')}
                  </div>
                ) : null}
                <p style={{ color: 'var(--text-dim)', fontSize: 13, marginTop: 6 }}>
                  {t('settings.promptHint')}
                </p>
              </div>
            ) : null}
          </section>

          <section className="section" style={{ gridColumn: '1 / -1' }}>
            <h2 className="section-title">{t('settings.soulTitle')}</h2>
            <p style={{ color: 'var(--text-dim)', fontSize: 13 }}>{t('settings.soulHint')}</p>
            <div className="tag-list" style={{ marginBottom: 12 }}>
              {SOUL_FILES.map((f) => (
                <button
                  key={f}
                  className={soulFile === f ? 'sm primary' : 'sm'}
                  onClick={() => setSoulFile(f)}
                >
                  {t('settings.soulOpen', { name: soulLabel(f) })}
                </button>
              ))}
            </div>
            {soulFile ? (
              <div className="card">
                {soul.isLoading ? (
                  <div className="loading">{t('common.loading')}</div>
                ) : soul.isError ? (
                  <div className="error-note">{t('settings.soulLoadFailed', { name: soulFile })}</div>
                ) : (
                  <>
                    <textarea
                      value={soulText}
                      onChange={(e) => setSoulText(e.target.value)}
                      rows={16}
                      style={{ width: '100%', fontFamily: 'var(--mono, monospace)', fontSize: 12.5 }}
                    />
                    <div className="row" style={{ marginTop: 10 }}>
                      <button
                        className="primary"
                        disabled={putSoul.isPending || soulText === soul.data?.content}
                        onClick={() => putSoul.mutate()}
                      >
                        {putSoul.isPending ? t('settings.saving') : t('settings.soulSave')}
                      </button>
                      {soul.data ? (
                        <span className="mono" style={{ fontSize: 12 }}>
                          mtime {soul.data.mtime}
                        </span>
                      ) : null}
                      <span className="spacer" />
                      <span className="mono" style={{ fontSize: 12 }}>
                        signed in as {auth.session?.username}
                      </span>
                    </div>
                    {soulMsg ? <div className="hint" style={{ marginTop: 8 }}>{soulMsg}</div> : null}
                  </>
                )}
              </div>
            ) : null}
          </section>
        </div>
      ) : null}
    </>
  )
}

function SecretRow({ k, v }: { k: string; v: string }) {
  return (
    <div className="row" style={{ justifyContent: 'space-between', padding: '4px 0' }}>
      <span style={{ color: 'var(--text-dim)', fontSize: 13 }}>{k}</span>
      <span className="mono" style={{ fontSize: 12.5 }}>{v}</span>
    </div>
  )
}
