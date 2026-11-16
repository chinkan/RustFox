import { useMutation, useQueryClient } from '@tanstack/react-query'
import { useEffect, useMemo, useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { Api } from '../api/client'
import type {
  SettingsPatch,
  VoiceEngine,
  VoiceLanguage,
  VoicePackDownload,
  VoiceSettings,
} from '../api/types'
import {
  normalizeVoiceSettings,
  sortVoiceLanguages,
  VOICE_LANGUAGE_OPTIONS,
  voicePackNeedsAttention,
  voiceSettingsDirty,
  type VoicePackKey,
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
  const [engine, setEngine] = useState<VoiceEngine>(server.engine)
  const [qwen3LicenseAccepted, setQwen3LicenseAccepted] = useState(server.qwen3LicenseAccepted)
  const [msg, setMsg] = useState<{ text: string; ok: boolean } | null>(null)

  useEffect(() => {
    setEnabled(server.enabled)
    setLanguages(server.languages)
    setTts(server.tts)
    setEngine(server.engine)
    setQwen3LicenseAccepted(server.qwen3LicenseAccepted)
  }, [server.enabled, server.languages, server.tts, server.engine, server.qwen3LicenseAccepted])

  const download = server.download
  const languagesEmpty = languages.length === 0
  const dirty = voiceSettingsDirty(server, enabled, languages, tts, engine, qwen3LicenseAccepted)
  const attention = voicePackNeedsAttention(enabled, tts, download, engine)

  const save = useMutation({
    mutationFn: () => {
      if (languagesEmpty) {
        return Promise.reject(new Error(t('settings.voiceLanguagesRequired')))
      }
      const patch: SettingsPatch = {
        voiceEnabled: enabled,
        voiceLanguages: sortVoiceLanguages(languages),
        voiceTts: tts,
        voiceEngine: engine,
        voiceQwen3AsrLicenseAccepted: qwen3LicenseAccepted,
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

  const packLabel = (pack: VoicePackKey): string => {
    switch (pack) {
      case 'sensevoice':
        return t('settings.voicePackSensevoice')
      case 'ttsEnglish':
        return t('settings.voicePackTtsEnglish')
      case 'ttsCantonese':
        return t('settings.voicePackTtsCantonese')
      case 'qwen3Asr':
        return t('settings.voicePackQwen3')
    }
  }

  const renderPack = (packKey: VoicePackKey, label: string) => {
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

        <label className="field" htmlFor="voice-engine">
          {t('settings.voiceEngine')}
        </label>
        <select
          id="voice-engine"
          value={engine}
          onChange={(e) => setEngine(e.target.value === 'qwen3-asr' ? 'qwen3-asr' : 'sensevoice')}
          style={{ marginBottom: 12 }}
        >
          <option value="sensevoice">{t('settings.voiceEngineSensevoice')}</option>
          <option value="qwen3-asr">{t('settings.voiceEngineQwen3')}</option>
        </select>

        <label style={{ display: 'flex', gap: 8, alignItems: 'center', margin: '12px 0' }}>
          <input type="checkbox" checked={tts} onChange={(e) => setTts(e.target.checked)} />
          {t('settings.voiceTts')}
        </label>

        <h3 style={{ margin: '0 0 8px', fontSize: 15 }}>{t('settings.voiceDownloadTitle')}</h3>
        {renderPack('sensevoice', t('settings.voicePackSensevoice'))}
        {renderPack('qwen3Asr', t('settings.voicePackQwen3'))}
        <label style={{ display: 'flex', gap: 8, alignItems: 'flex-start', margin: '4px 0 12px' }}>
          <input
            type="checkbox"
            checked={qwen3LicenseAccepted}
            onChange={(e) => setQwen3LicenseAccepted(e.target.checked)}
          />
          <span>
            {t('settings.voiceQwen3License')}{' '}
            <a
              href="https://raw.githubusercontent.com/QwenLM/Qwen3-ASR/main/LICENSE"
              target="_blank"
              rel="noreferrer"
            >
              {t('settings.voiceQwen3LicenseLink')}
            </a>
            . {t('settings.voiceQwen3LicenseHint')}
          </span>
        </label>
        {renderPack('ttsCantonese', t('settings.voicePackTtsCantonese'))}
        {renderPack('ttsEnglish', t('settings.voicePackTtsEnglish'))}

        {attention ? (
          <div className="banner warn" role="status" style={{ marginTop: 12 }}>
            {t('settings.voicePackWarn', {
              pack: packLabel(attention.pack),
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
