import { createFileRoute } from '@tanstack/react-router'
import { useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { ApiError } from '../../api/client'

export const Route = createFileRoute('/_auth/triggers')({
  component: TriggersPage,
})

function webhookUrl(path: string): string {
  if (typeof window === 'undefined') return path
  return `${window.location.origin}${path}`
}

function TriggersPage() {
  const { api } = Route.useRouteContext()
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const triggers = useQuery({ queryKey: ['triggers'], queryFn: api.listTriggers })
  const [notice, setNotice] = useState<string | null>(null)
  const [secretOnce, setSecretOnce] = useState<string | null>(null)
  const [creating, setCreating] = useState(false)
  const [name, setName] = useState('')
  const [prompt, setPrompt] = useState('')
  const [botId, setBotId] = useState('')

  const errText = (e: unknown) =>
    e instanceof ApiError ? `${e.code}: ${e.message}` : (e as Error).message
  const invalidate = () => void queryClient.invalidateQueries({ queryKey: ['triggers'] })

  const create = useMutation({
    mutationFn: () =>
      api.createTrigger({
        name: name.trim(),
        prompt: prompt.trim(),
        botId: botId.trim() || undefined,
      }),
    onSuccess: (res) => {
      setNotice(t('triggers.created'))
      setSecretOnce(res.secret ?? null)
      setCreating(false)
      setName('')
      setPrompt('')
      invalidate()
    },
    onError: (e) => setNotice(errText(e)),
  })

  const toggle = useMutation({
    mutationFn: ({ id, enabled }: { id: string; enabled: boolean }) =>
      enabled ? api.disableTrigger(id) : api.enableTrigger(id),
    onSuccess: () => invalidate(),
    onError: (e) => setNotice(errText(e)),
  })

  const rotate = useMutation({
    mutationFn: (id: string) => api.rotateTriggerSecret(id),
    onSuccess: (res) => {
      setSecretOnce(res.secret ?? null)
      setNotice(t('triggers.rotated'))
    },
    onError: (e) => setNotice(errText(e)),
  })

  const bots = triggers.data?.bots ?? []
  const rows = triggers.data?.triggers ?? []
  const formOk = name.trim() !== '' && prompt.trim() !== '' && (bots.length <= 1 || botId !== '')

  return (
    <>
      <div className="page-head">
        <h1>{t('triggers.title')}</h1>
        <p>{t('triggers.subtitle')}</p>
      </div>
      {notice ? <div className="hint" style={{ marginBottom: 12 }}>{notice}</div> : null}
      {secretOnce ? (
        <section className="section">
          <h2 className="section-title">{t('triggers.secretOnce')}</h2>
          <div className="card">
            <code>{secretOnce}</code>
          </div>
        </section>
      ) : null}
      <div className="row" style={{ marginBottom: 12 }}>
        <span className="spacer" />
        <button className="ghost sm" type="button" onClick={() => setCreating((open) => !open)}>
          {creating ? t('common.cancel') : t('triggers.new')}
        </button>
      </div>
      {creating ? (
        <section className="section">
          <div className="card">
            <div className="field">
              <label htmlFor="trigger-name">{t('triggers.name')}</label>
              <input id="trigger-name" value={name} onChange={(e) => setName(e.target.value)} />
            </div>
            <div className="field">
              <label htmlFor="trigger-prompt">{t('triggers.prompt')}</label>
              <textarea
                id="trigger-prompt"
                value={prompt}
                onChange={(e) => setPrompt(e.target.value)}
              />
            </div>
            {bots.length > 1 ? (
              <div className="field">
                <label htmlFor="trigger-bot">{t('triggers.bot')}</label>
                <select id="trigger-bot" value={botId} onChange={(e) => setBotId(e.target.value)}>
                  <option value="">{t('triggers.bot')}</option>
                  {bots.map((bot) => (
                    <option key={bot.id} value={bot.id}>
                      {bot.id}
                    </option>
                  ))}
                </select>
              </div>
            ) : null}
            <button
              type="button"
              disabled={!formOk || create.isPending}
              onClick={() => create.mutate()}
            >
              {t('triggers.create')}
            </button>
          </div>
        </section>
      ) : null}
      {rows.length === 0 ? <p className="hint">{t('triggers.empty')}</p> : null}
      {rows.map((row) => (
        <section className="section" key={row.id}>
          <div className="card">
            <h2 className="section-title">{row.name}</h2>
            <p>{row.prompt}</p>
            <p>
              <span className="hint">{t('triggers.webhook')}</span>
              <br />
              <code>{webhookUrl(row.webhookPath)}</code>
            </p>
            <p className="hint">
              {row.botId} · {row.secretName} · {row.enabled ? t('common.enabled') : t('common.paused')}
            </p>
            <div className="row">
              <button
                type="button"
                className="ghost sm"
                onClick={() => toggle.mutate({ id: row.id, enabled: row.enabled })}
              >
                {row.enabled ? t('triggers.disable') : t('triggers.enable')}
              </button>
              <button type="button" className="ghost sm" onClick={() => rotate.mutate(row.id)}>
                {t('triggers.rotate')}
              </button>
            </div>
          </div>
        </section>
      ))}
    </>
  )
}
