import { useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { Api } from '../api/client'

export function DevicesSection({ api, hideTitle }: { api: Api; hideTitle?: boolean }) {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const devices = useQuery({ queryKey: ['devices'], queryFn: api.listDevices })
  const [label, setLabel] = useState('')
  const [room, setRoom] = useState('')
  const [once, setOnce] = useState<string | null>(null)
  const [msg, setMsg] = useState<{ text: string; ok: boolean } | null>(null)
  const [busy, setBusy] = useState(false)

  const invalidate = () => {
    void queryClient.invalidateQueries({ queryKey: ['devices'] })
  }

  const run = async (fn: () => Promise<string | void>, okMsg: string) => {
    setBusy(true)
    try {
      const token = await fn()
      setOnce(token ?? null)
      setMsg({ ok: true, text: okMsg })
      invalidate()
    } catch (e) {
      setMsg({ ok: false, text: t('settings.devicesFailed', { message: (e as Error).message }) })
    } finally {
      setBusy(false)
    }
  }

  return (
    <section className="section" style={{ gridColumn: '1 / -1' }}>
      {hideTitle ? null : (
        <h2 className="section-title">{t('settings.devicesTitle')}</h2>
      )}
      <p className="hint" style={{ marginBottom: 12 }}>{t('settings.devicesHint')}</p>
      {devices.data && !devices.data.enabled ? (
        <div className="banner warn" role="status" style={{ marginBottom: 12 }}>
          {t('settings.devicesOff')}
        </div>
      ) : null}
      {once ? (
        <div className="banner warn" role="status" style={{ marginBottom: 12 }}>
          <div>{t('settings.devicesTokenOnce')}</div>
          <code className="mono">{once}</code>
        </div>
      ) : null}
      <div className="card" style={{ marginBottom: 12 }}>
        <div className="field">
          <label htmlFor="settings-device-label">{t('settings.devicesLabel')}</label>
          <input
            id="settings-device-label"
            type="text"
            value={label}
            disabled={busy}
            onChange={(e) => setLabel(e.target.value)}
          />
        </div>
        <div className="field">
          <label htmlFor="settings-device-room">{t('settings.devicesRoom')}</label>
          <input
            id="settings-device-room"
            type="text"
            value={room}
            disabled={busy}
            onChange={(e) => setRoom(e.target.value)}
          />
        </div>
        <button
          type="button"
          className="primary"
          disabled={busy || label.trim() === ''}
          onClick={() => {
            const nextLabel = label.trim()
            const nextRoom = room.trim()
            void run(async () => {
              const issued = await api.createDevice({
                label: nextLabel,
                ...(nextRoom ? { room: nextRoom } : {}),
              })
              setLabel('')
              setRoom('')
              return issued.token
            }, t('settings.devicesCreated'))
          }}
        >
          {t('settings.devicesCreate')}
        </button>
      </div>
      {devices.isLoading ? (
        <div className="loading">{t('common.loading')}</div>
      ) : devices.isError ? (
        <div className="error-note">{(devices.error as Error).message}</div>
      ) : (devices.data?.devices.length ?? 0) === 0 ? (
        <div className="hint">{t('settings.devicesEmpty')}</div>
      ) : (
        <ul style={{ listStyle: 'none', padding: 0, margin: 0 }}>
          {devices.data?.devices.map((d) => (
            <li key={d.id} className="row" style={{ justifyContent: 'space-between', gap: 8, marginBottom: 8 }}>
              <span>
                <strong>{d.label}</strong>
                {d.room ? <span className="hint"> · {d.room}</span> : null}
              </span>
              <span className="row" style={{ gap: 8 }}>
                <button
                  type="button"
                  className="sm"
                  disabled={busy}
                  onClick={() => {
                    void run(
                      async () => (await api.rotateDevice(d.id)).token,
                      t('settings.devicesRotated'),
                    )
                  }}
                >
                  {t('settings.devicesRotate')}
                </button>
                <button
                  type="button"
                  className="sm"
                  disabled={busy}
                  onClick={() => {
                    if (!window.confirm(t('settings.devicesRevokeConfirm', { label: d.label }))) return
                    void run(async () => {
                      await api.revokeDevice(d.id)
                    }, t('settings.devicesRevoked'))
                  }}
                >
                  {t('settings.devicesRevoke')}
                </button>
              </span>
            </li>
          ))}
        </ul>
      )}
      {msg ? (
        <div className={msg.ok ? 'hint' : 'error-note'} role="status" style={{ marginTop: 12 }}>
          {msg.text}
        </div>
      ) : null}
    </section>
  )
}
