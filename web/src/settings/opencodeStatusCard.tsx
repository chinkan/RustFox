import { useQuery } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import type { Api } from '../api/client'

/**
 * Read-only OpenCode probe (ADR-0043 Phase 1). Three states only.
 * No provider radio, no Active picker, no enable/save.
 */
export function OpenCodeStatusCard({ api }: { api: Api }) {
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
