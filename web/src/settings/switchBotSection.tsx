import { useMutation, useQueryClient } from '@tanstack/react-query'
import { useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { Api } from '../api/client'
import type { SettingsPatch, SwitchBotSettings } from '../api/types'

export function SwitchBotSection({
  api,
  switchbot,
}: {
  api: Api
  switchbot: SwitchBotSettings
}) {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const [enabled, setEnabled] = useState(switchbot.enabled)
  const [token, setToken] = useState('')
  const [secret, setSecret] = useState('')
  const [msg, setMsg] = useState<{ text: string; ok: boolean } | null>(null)

  useEffect(() => {
    setEnabled(switchbot.enabled)
  }, [switchbot.enabled])

  const save = useMutation({
    mutationFn: () => {
      const patch: SettingsPatch = { switchbotEnabled: enabled }
      if (token.trim()) patch.switchbotToken = token.trim()
      if (secret.trim()) patch.switchbotSecret = secret.trim()
      return api.patchSettings(patch)
    },
    onSuccess: () => {
      setToken('')
      setSecret('')
      setMsg({ text: t('settings.saved'), ok: true })
      void queryClient.invalidateQueries({ queryKey: ['settings'] })
    },
    onError: (e: Error) => {
      setMsg({ text: t('settings.saveFailed', { message: e.message }), ok: false })
    },
  })

  return (
    <section className="section" style={{ gridColumn: '1 / -1' }}>
      <h2 className="section-title">{t('settings.switchbotTitle')}</h2>
      <p className="hint" style={{ marginBottom: 12 }}>{t('settings.switchbotHint')}</p>
      <div className="card">
        <label style={{ display: 'flex', gap: 8, alignItems: 'center', marginBottom: 12 }}>
          <input
            type="checkbox"
            checked={enabled}
            onChange={(e) => setEnabled(e.target.checked)}
          />
          {t('settings.switchbotEnable')}
        </label>
        <div className="field">
          <label htmlFor="switchbot-token">{t('settings.switchbotToken')}</label>
          <input
            id="switchbot-token"
            type="password"
            autoComplete="off"
            value={token}
            placeholder={switchbot.tokenMasked ?? t('settings.switchbotNotSet')}
            onChange={(e) => setToken(e.target.value)}
          />
        </div>
        <div className="field">
          <label htmlFor="switchbot-secret">{t('settings.switchbotSecret')}</label>
          <input
            id="switchbot-secret"
            type="password"
            autoComplete="off"
            value={secret}
            placeholder={switchbot.secretMasked ?? t('settings.switchbotNotSet')}
            onChange={(e) => setSecret(e.target.value)}
          />
        </div>
        <div className="row">
          <button className="primary" disabled={save.isPending} onClick={() => save.mutate()}>
            {save.isPending ? t('settings.saving') : t('settings.save')}
          </button>
          {msg ? (
            <span className={msg.ok ? 'badge dot running' : 'error-note'}>{msg.text}</span>
          ) : null}
        </div>
      </div>
    </section>
  )
}
