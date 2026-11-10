import { useMutation, useQueryClient } from '@tanstack/react-query'
import { useEffect, useMemo, useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { Api } from '../api/client'
import type { SettingsPatch, VoiceLanguage, VoicePackDownload, VoiceSettings } from '../api/types'
import {
  normalizeVoiceSettings,
  sortVoiceLanguages,
  VOICE_LANGUAGE_OPTIONS,
  voicePackNeedsAttention,
  voiceSettingsDirty,
} from './voice'

function packBadgeClass(status: VoicePackDownload['status']): string {
  switch (status) {
    case 'ready':
      return 'badge dot running'
    case 'downloading':
      return 'badge'
    case 'error':
      return 'badge error'
    default:
      return 'badge'
  }
}

export function VoiceSection({
  api,
  voice: rawVoice,
}: {
  api: Api
  voice?: VoiceSettings | null
}) {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const server = useMemo(() => normalizeVoiceSettings(rawVoice), [rawVoice])

  const [enabled, setEnabled] = useState(server.enabled)
  const [languages, setLanguages] = useState<VoiceLanguage[]>(server.languages)
  const [tts, setTts] = useState(server.tts)
  const [msg, setMsg] = useState<{ text: string; ok: boolean } | null>(null)

  useEffect(() => {
    setEnabled(server.enabled)
    setLanguages(server.languages)
    setTts(server.tts)
  }, [server.enabled, server.languages, server.tts])

  const download = server.download
  const languagesEmpty = languages.length === 0
  const dirty = voiceSettingsDirty(server, enabled, languages, tts)
  const attention = voicePackNeedsAttention(enabled, tts, download)

  const save = useMutation({
    mutationFn: () => {
      if (languagesEmpty) {
        return Promise.reject(new Error(t('settings.voiceLanguagesRequired')))
      }
      const patch: SettingsPatch = {
        voiceEnabled: enabled,
        voiceLanguages: sortVoiceLanguages(languages),
        voiceTts: tts,
      }
      return api.patchSettings(patch)
    },
    onSuccess: () => {
      setMsg({ text: t('settings.saved'), ok: true })
      void queryClient.invalidateQueries({ queryKey: ['settings'] })
    },
    onError: (e: Error) => {
      setMsg({ text: t('settings.saveFailed', { message: e.message }), ok: false })
    },
  })

  const toggleLanguage = (id: VoiceLanguage, on: boolean) => {
    setLanguages((prev) => {
      if (on) return sortVoiceLanguages([...prev, id])
      return prev.filter((x) => x !== id)
    })
  }

  const renderPack = (packKey: 'sensevoice' | 'ttsCantonese', label: string) => {
    const pack = download[packKey]
    const statusLabel = t(`settings.voiceDownloadStatus.${pack.status}`)
    return (
      <div className="field" key={packKey}>
        <div className="row" style={{ justifyContent: 'space-between', gap: 8, flexWrap: 'wrap' }}>
          <label>{label}</label>
          <span className={packBadgeClass(pack.status)} role="status">
            {statusLabel}
          </span>
        </div>
        {pack.message ? (
          <div
            className={pack.status === 'error' ? 'error-note' : 'hint'}
            role="status"
            style={{ marginTop: 4 }}
          >
            {pack.message}
          </div>
        ) : null}
      </div>
    )
  }

  return (
    <section className="section" style={{ gridColumn: '1 / -1' }}>
      <h2 className="section-title">{t('settings.voiceTitle')}</h2>
      <p className="hint" style={{ marginBottom: 12 }}>{t('settings.voiceHint')}</p>
      <div className="card">
        <label style={{ display: 'flex', gap: 8, alignItems: 'center', marginBottom: 12 }}>
          <input
            type="checkbox"
            checked={enabled}
            onChange={(e) => setEnabled(e.target.checked)}
          />
          {t('settings.voiceEnable')}
        </label>

        <fieldset className="field" style={{ border: 'none', margin: 0, padding: 0 }}>
          <legend style={{ marginBottom: 8 }}>{t('settings.voiceLanguages')}</legend>
          <div className="tag-list" role="group" aria-label={t('settings.voiceLanguages')}>
            {VOICE_LANGUAGE_OPTIONS.map((id) => (
              <label key={id} style={{ display: 'inline-flex', gap: 6, alignItems: 'center', marginRight: 12 }}>
                <input
                  type="checkbox"
                  checked={languages.includes(id)}
                  onChange={(e) => toggleLanguage(id, e.target.checked)}
                />
                {t(`settings.voiceLang.${id}`)}
              </label>
            ))}
          </div>
          {languagesEmpty ? (
            <div className="error-note" role="alert" style={{ marginTop: 8 }}>
              {t('settings.voiceLanguagesRequired')}
            </div>
          ) : null}
        </fieldset>

        <label style={{ display: 'flex', gap: 8, alignItems: 'center', margin: '12px 0' }}>
          <input type="checkbox" checked={tts} onChange={(e) => setTts(e.target.checked)} />
          {t('settings.voiceTts')}
        </label>

        <h3 style={{ margin: '0 0 8px', fontSize: 15 }}>{t('settings.voiceDownloadTitle')}</h3>
        {renderPack('sensevoice', t('settings.voicePackSensevoice'))}
        {renderPack('ttsCantonese', t('settings.voicePackTtsCantonese'))}

        {attention ? (
          <div className="banner warn" role="status" style={{ marginTop: 12 }}>
            {t('settings.voicePackWarn', {
              pack:
                attention.pack === 'sensevoice'
                  ? t('settings.voicePackSensevoice')
                  : t('settings.voicePackTtsCantonese'),
              status: t(`settings.voiceDownloadStatus.${attention.status}`),
            })}
          </div>
        ) : null}

        <div className="row" style={{ marginTop: 12 }}>
          <button
            className="primary"
            disabled={!dirty || languagesEmpty || save.isPending}
            onClick={() => {
              setMsg(null)
              save.mutate()
            }}
          >
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
