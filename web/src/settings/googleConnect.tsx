import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'

export function GoogleConnect({
  api,
  portalPort = 8090,
}: {
  api: {
    googleStatus: () => Promise<{ offered: boolean }>
    startGoogle: () => Promise<{ offered: boolean; authorizeUrl?: string }>
    setGoogleClientId: (clientId: string) => Promise<{ offered: boolean }>
  }
  portalPort?: number
}) {
  const { t } = useTranslation()
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
  const callbackUrl = `http://127.0.0.1:${portalPort}/api/connectors/google/callback`

  return (
    <section className="section" aria-label={t('googleConnect.title')}>
      <h2 className="section-title">{t('googleConnect.title')}</h2>
      <div className="card">
        <p className="hint" style={{ marginBottom: 12 }}>
          {t('googleConnect.intro')}
        </p>
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
            {t('googleConnect.connect')}
          </button>
        ) : (
          <div className="hint" role="status">
            {t('googleConnect.noClientId')}
          </div>
        )}
        <details style={{ marginTop: 12 }}>
          <summary>{t('googleConnect.advancedSummary')}</summary>
          <p className="hint" style={{ marginTop: 8 }}>
            {t('googleConnect.advancedWhen')}
          </p>
          <p className="hint" style={{ marginTop: 8 }}>
            {t('googleConnect.desktopIdHelp')}
          </p>
          <p className="hint" style={{ marginTop: 8 }}>
            {t('googleConnect.redirectHelp', { portalPort })}
          </p>
          <code className="mono" style={{ display: 'block', fontSize: 12, marginTop: 4 }}>
            {callbackUrl}
          </code>
          <div className="field" style={{ marginTop: 12 }}>
            <label htmlFor="google-desktop-client-id">{t('googleConnect.desktopIdLabel')}</label>
            <input
              id="google-desktop-client-id"
              type="password"
              autoComplete="off"
              value={clientId}
              onChange={(e) => setClientId(e.target.value)}
            />
            <span className="hint">{t('googleConnect.saveEmptyHint')}</span>
          </div>
          <button
            type="button"
            className="btn-secondary"
            onClick={() => saveId.mutate()}
            disabled={saveId.isPending}
          >
            {saveId.isPending ? t('googleConnect.saving') : t('googleConnect.save')}
          </button>
        </details>
      </div>
    </section>
  )
}
