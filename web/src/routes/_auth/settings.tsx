import { createFileRoute, redirect } from '@tanstack/react-router'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { ApiError } from '../../api/client'
import type { Api } from '../../api/client'
import type {
  AutonomyMode,
  McpPreset,
  McpServerMasked,
  McpUpsertBody,
  SelfUpgradeChoice,
  SettingsPatch,
  SoulName,
} from '../../api/types'
import { ensureAuthBootstrapped } from '../../auth'
import { OfficePrefsSection } from '../../agents/officePrefs'

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

function GoogleConnect({ api }: { api: { googleStatus: () => Promise<{ offered: boolean }>; startGoogle: () => Promise<{ offered: boolean; authorizeUrl?: string }>; setGoogleClientId: (clientId: string) => Promise<{ offered: boolean }> } }) {
  const queryClient = useQueryClient()
  const google = useQuery({ queryKey: ['google-connector'], queryFn: api.googleStatus })
  const [clientId, setClientId] = useState('')
  const saveId = useMutation({
    mutationFn: () => api.setGoogleClientId(clientId),
    onSuccess: () => {
      setClientId('')
      void queryClient.invalidateQueries({ queryKey: ['google-connector'] })
    },
  })
  return (
    <section className="section">
      <h2 className="section-title">Google</h2>
      <div className="card">
        {google.data?.offered ? (
          <button
            type="button"
            className="primary"
            onClick={() => {
              void api.startGoogle().then((res) => {
                if (res.offered && res.authorizeUrl) window.location.href = res.authorizeUrl
              })
            }}
          >
            Google
          </button>
        ) : null}
        <details style={{ marginTop: 12 }}>
          <summary>Advanced</summary>
          <div className="field">
            <label>Desktop client id</label>
            <input
              type="password"
              autoComplete="off"
              value={clientId}
              onChange={(e) => setClientId(e.target.value)}
            />
          </div>
          <button type="button" className="btn-secondary" onClick={() => saveId.mutate()} disabled={saveId.isPending}>
            Save
          </button>
        </details>
      </div>
    </section>
  )
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
  const [sandboxMsg, setSandboxMsg] = useState<{ text: string; ok: boolean } | null>(null)
  const [choice, setChoice] = useState<SelfUpgradeChoice>('auto')
  const [upgradeMsg, setUpgradeMsg] = useState<{ text: string; ok: boolean } | null>(null)

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

  const switchSafe = useMutation({
    mutationFn: () => api.switchSandboxSafe(),
    onSuccess: (r) => {
      setSandboxMsg({ text: r.message, ok: r.ok })
      void queryClient.invalidateQueries({ queryKey: ['settings'] })
    },
    onError: (e: Error) => setSandboxMsg({ text: e.message, ok: false }),
  })
  const switchTrusted = useMutation({
    mutationFn: () => api.switchSandboxTrusted(),
    onSuccess: (r) => {
      setSandboxMsg({ text: r.message, ok: r.ok })
      void queryClient.invalidateQueries({ queryKey: ['settings'] })
    },
    onError: (e: Error) => setSandboxMsg({ text: e.message, ok: false }),
  })
  const installSandbox = useMutation({
    mutationFn: () => api.installSandboxTool(),
    onSuccess: (r) => {
      setSandboxMsg({ text: r.message, ok: r.ok })
      void queryClient.invalidateQueries({ queryKey: ['settings'] })
    },
    onError: (e: Error) => setSandboxMsg({ text: e.message, ok: false }),
  })

  const selfUpgrade = useMutation({
    mutationFn: () => api.selfUpgrade(choice),
    onSuccess: () => {
      setUpgradeMsg({ text: t('settings.upgradeSuccess'), ok: true })
    },
    onError: (e: Error) => {
      if (e instanceof ApiError && e.code === 'invalid_selfupgrade_choice') {
        setUpgradeMsg({ text: t('settings.upgradeRejectUnknown'), ok: false })
        return
      }
      setUpgradeMsg({ text: t('settings.upgradeFailed', { message: e.message }), ok: false })
    },
  })

  const editable = settings.data?.editable
  const masked = settings.data?.masked
  const promptInfo = settings.data?.systemPrompt
  const sandbox = settings.data?.sandbox
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

      <OfficePrefsSection />

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

          <GoogleConnect api={api} />

          <section className="section">
            <h2 className="section-title">{t('settings.maskedTitle')}</h2>
            <div className="card">
              <SecretRow k={t('settings.telegramBotToken')} v={masked?.telegramBotToken ?? ''} />
              <SecretRow k={t('settings.openrouterApiKey')} v={masked?.openrouterApiKey ?? ''} />
              {masked?.xaiApiKey ? (
                <SecretRow k={t('settings.xaiApiKey')} v={masked.xaiApiKey} />
              ) : null}
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
            </div>
          </section>

          <OpenCodeStatusCard api={api} />

          <McpSettingsSection api={api} servers={masked?.mcpServers ?? []} />

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
            <h2 className="section-title">{t('settings.sandboxTitle')}</h2>
            <div className="card">
              {sandbox ? (
                <>
                  <p style={{ fontSize: 13 }}>
                    {t('settings.sandboxCurrentMode', {
                      mode:
                        sandbox.mode === 'sandbox'
                          ? t('settings.sandboxModeSafe')
                          : t('settings.sandboxModeTrusted'),
                    })}
                  </p>
                  {sandbox.blockedBySystem || sandbox.statusMessage ? (
                    <div className="error-note" role="status" style={{ marginTop: 8 }}>
                      {sandbox.statusMessage
                        ?? "This computer's system settings block safe mode, so RustFox will run in trusted mode."}
                    </div>
                  ) : null}
                  {sandbox.windowsNote ? (
                    <div className="hint" role="status" style={{ marginTop: 8 }}>
                      {t('settings.sandboxWindowsNote')}
                    </div>
                  ) : null}
                  <div className="row" style={{ marginTop: 12, gap: 8, flexWrap: 'wrap' }}>
                    {sandbox.mode !== 'sandbox' && sandbox.platform !== 'windows' && !sandbox.blockedBySystem ? (
                      <button
                        type="button"
                        className="primary"
                        disabled={switchSafe.isPending}
                        onClick={() => {
                          setSandboxMsg(null)
                          switchSafe.mutate()
                        }}
                      >
                        {switchSafe.isPending ? t('settings.saving') : t('settings.sandboxSwitchSafe')}
                      </button>
                    ) : null}
                    {sandbox.mode === 'sandbox' ? (
                      <button
                        type="button"
                        className="btn-secondary"
                        disabled={switchTrusted.isPending}
                        onClick={() => {
                          setSandboxMsg(null)
                          switchTrusted.mutate()
                        }}
                      >
                        {t('settings.sandboxSwitchTrusted')}
                      </button>
                    ) : null}
                    {sandbox.canInstall && sandbox.mode !== 'sandbox' ? (
                      <button
                        type="button"
                        className="btn-secondary"
                        disabled={installSandbox.isPending}
                        onClick={() => {
                          setSandboxMsg({
                            text: t('settings.sandboxInstallingHint'),
                            ok: true,
                          })
                          installSandbox.mutate()
                        }}
                      >
                        {installSandbox.isPending
                          ? t('settings.sandboxInstallingHint')
                          : t('settings.sandboxInstallButton')}
                      </button>
                    ) : null}
                  </div>
                  {!sandbox.canInstall
                    && !sandbox.blockedBySystem
                    && sandbox.installCommand
                    && sandbox.platform === 'linux'
                    && sandbox.mode !== 'sandbox' ? (
                    <div className="hint" style={{ marginTop: 8 }}>
                      <code>{sandbox.installCommand}</code>
                    </div>
                  ) : null}
                  {sandboxMsg ? (
                    <div
                      className={sandboxMsg.ok ? 'hint' : 'error-note'}
                      style={{ marginTop: 8 }}
                    >
                      {sandboxMsg.text}
                    </div>
                  ) : null}
                </>
              ) : (
                <div className="loading">{t('common.loading')}</div>
              )}
            </div>
          </section>

          <section className="section" style={{ gridColumn: '1 / -1' }}>
            <h2 className="section-title">{t('settings.upgradeTitle')}</h2>
            <div className="card">
              <p className="hint" style={{ marginBottom: 12 }}>{t('settings.upgradeHint')}</p>
              <div className="field">
                <label htmlFor="settings-self-upgrade-mode">{t('settings.upgradeMode')}</label>
                <select
                  id="settings-self-upgrade-mode"
                  value={choice}
                  onChange={(e) => setChoice(e.target.value as SelfUpgradeChoice)}
                  disabled={selfUpgrade.isPending}
                  style={{ maxWidth: 320 }}
                >
                  <option value="auto">{t('settings.upgradeModeAuto')}</option>
                  <option value="main">{t('settings.upgradeModeMain')}</option>
                  <option value="release">{t('settings.upgradeModeRelease')}</option>
                </select>
              </div>
              <div className="row" style={{ gap: 8 }}>
                <button
                  type="button"
                  className="primary"
                  disabled={selfUpgrade.isPending}
                  onClick={() => {
                    setUpgradeMsg(null)
                    selfUpgrade.mutate()
                  }}
                >
                  {selfUpgrade.isPending ? t('settings.upgradeRunning') : t('settings.upgradeRun')}
                </button>
              </div>
              {selfUpgrade.isPending ? (
                <div className="hint" role="status" style={{ marginTop: 8 }}>
                  {t('settings.upgradeRunning')}
                </div>
              ) : null}
              {upgradeMsg ? (
                <div
                  className={upgradeMsg.ok ? 'hint' : 'error-note'}
                  role="status"
                  style={{ marginTop: 8 }}
                >
                  {upgradeMsg.text}
                </div>
              ) : null}
            </div>
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

/**
 * Read-only OpenCode probe (ADR-0043 Phase 1). Three states only.
 * No provider radio, no Active picker, no enable/save.
 */
function OpenCodeStatusCard({ api }: { api: Api }) {
  const { t } = useTranslation()
  const status = useQuery({
    queryKey: ['opencode-status'],
    queryFn: () => api.getOpenCodeStatus(),
  })
  const data = status.data
  const badgeClass =
    data?.state === 'ready'
      ? 'badge dot running'
      : data?.state === 'no_creds'
        ? 'badge'
        : 'badge error'
  const badgeLabel =
    data?.state === 'ready'
      ? t('settings.opencodeReady')
      : data?.state === 'no_creds'
        ? t('settings.opencodeNoCreds')
        : data?.state === 'absent'
          ? t('settings.opencodeAbsent')
          : (data?.state ?? '')

  return (
    <section className="section" aria-label={t('settings.opencodeTitle')}>
      <h2 className="section-title">{t('settings.opencodeTitle')}</h2>
      <div className="card">
        {status.isLoading ? (
          <div className="loading">{t('common.loading')}</div>
        ) : status.isError ? (
          <div className="error-note" role="status">
            {(status.error as Error).message}
          </div>
        ) : data ? (
          <>
            <div className="row" style={{ justifyContent: 'space-between', gap: 8 }}>
              <div className="tag-list" role="status">
                <span className={badgeClass}>{badgeLabel}</span>
                {data.state === 'ready' ? (
                  <span className="badge">
                    {t('settings.opencodeModelCount', { count: data.model_count })}
                  </span>
                ) : null}
              </div>
              <button
                type="button"
                className="sm"
                disabled={status.isFetching}
                onClick={() => {
                  void status.refetch()
                }}
              >
                {t('settings.opencodeRefresh')}
              </button>
            </div>
            {data.message ? (
              <div
                className={data.state === 'absent' ? 'error-note' : 'hint'}
                role="status"
                style={{ marginTop: 8 }}
              >
                {data.message}
              </div>
            ) : null}
            {data.state === 'ready' && data.models.length > 0 ? (
              <details style={{ marginTop: 8 }}>
                <summary>{t('settings.opencodeModels')}</summary>
                <ul style={{ margin: '8px 0 0', paddingLeft: 18 }}>
                  {data.models.map((id) => (
                    <li key={id} className="mono" style={{ fontSize: 12 }}>
                      {id}
                    </li>
                  ))}
                </ul>
              </details>
            ) : null}
          </>
        ) : null}
      </div>
    </section>
  )
}

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

function McpSettingsSection({
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


function SecretRow({ k, v }: { k: string; v: string }) {
  return (
    <div className="row" style={{ justifyContent: 'space-between', padding: '4px 0' }}>
      <span style={{ color: 'var(--text-dim)', fontSize: 13 }}>{k}</span>
      <span className="mono" style={{ fontSize: 12.5 }}>{v}</span>
    </div>
  )
}
