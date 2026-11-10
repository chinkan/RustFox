import type { VoiceDownload, VoiceLanguage, VoicePackDownload, VoiceSettings } from '../api/types'

/** SenseVoice language ids (ADR-0063). Display order matches storage order. */
export const VOICE_LANGUAGE_OPTIONS: readonly VoiceLanguage[] = ['yue', 'zh', 'en', 'ja', 'ko']

const LANGUAGE_SET = new Set<string>(VOICE_LANGUAGE_OPTIONS)

export function isVoiceLanguage(id: string): id is VoiceLanguage {
  return LANGUAGE_SET.has(id)
}

/** Collapse duplicates and sort to ADR canonical order. */
export function sortVoiceLanguages(ids: readonly string[]): VoiceLanguage[] {
  const picked = new Set<VoiceLanguage>()
  for (const id of VOICE_LANGUAGE_OPTIONS) {
    if (ids.includes(id)) picked.add(id)
  }
  return [...picked]
}

const defaultDownloadPack = (): VoicePackDownload => ({ status: 'absent', message: null })

const defaultDownload = (): VoiceDownload => ({
  sensevoice: defaultDownloadPack(),
  ttsCantonese: defaultDownloadPack(),
  ttsEnglish: defaultDownloadPack(),
})

/** GET may omit `voiceDownload` or one pack key; each pack still has a safe object. */
type VoiceSettingsInput = Omit<VoiceSettings, 'download'> & {
  download?: Partial<VoiceDownload> | null
}

function normalizeDownload(download?: Partial<VoiceDownload> | null): VoiceDownload {
  const fallback = defaultDownload()
  if (!download) return fallback
  return {
    sensevoice: download.sensevoice ?? fallback.sensevoice,
    ttsCantonese: download.ttsCantonese ?? fallback.ttsCantonese,
    ttsEnglish: download.ttsEnglish ?? fallback.ttsEnglish,
  }
}

/**
 * Live GET /api/settings has no nested `voice`. Writable fields live on
 * `editable`; pack status is top-level `voiceDownload`.
 */
export function voiceFromSettings(settings?: {
  editable?: {
    voiceEnabled?: boolean
    voiceLanguages?: readonly string[] | null
    voiceTts?: boolean
  } | null
  voiceDownload?: Partial<VoiceDownload> | null
} | null): VoiceSettings {
  if (!settings) return normalizeVoiceSettings(undefined)
  const editable = settings.editable
  const rawLanguages = editable?.voiceLanguages
  const languages =
    rawLanguages && rawLanguages.length > 0 ? sortVoiceLanguages(rawLanguages) : []
  return normalizeVoiceSettings({
    enabled: editable?.voiceEnabled ?? true,
    languages: languages.length > 0 ? languages : ['yue', 'en'],
    tts: editable?.voiceTts ?? false,
    download: settings.voiceDownload,
  })
}

/** Defaults when GET omits voice (enabled absent → true; languages → yue+en). */
export function normalizeVoiceSettings(voice?: VoiceSettingsInput | null): VoiceSettings {
  if (!voice) {
    return {
      enabled: true,
      languages: ['yue', 'en'],
      tts: false,
      download: defaultDownload(),
    }
  }
  const languages =
    voice.languages.length > 0
      ? sortVoiceLanguages(voice.languages)
      : (['yue', 'en'] as VoiceLanguage[])
  return {
    enabled: voice.enabled,
    languages,
    tts: voice.tts,
    download: normalizeDownload(voice.download),
  }
}

export function voiceLanguagesEqual(a: readonly VoiceLanguage[], b: readonly VoiceLanguage[]): boolean {
  if (a.length !== b.length) return false
  const sortedA = sortVoiceLanguages(a)
  const sortedB = sortVoiceLanguages(b)
  return sortedA.every((id, i) => id === sortedB[i])
}

export function voiceSettingsDirty(
  server: VoiceSettings,
  enabled: boolean,
  languages: VoiceLanguage[],
  tts: boolean,
): boolean {
  return (
    enabled !== server.enabled ||
    tts !== server.tts ||
    !voiceLanguagesEqual(languages, server.languages)
  )
}

export type VoicePackKey = 'sensevoice' | 'ttsCantonese' | 'ttsEnglish'

/** Fail-soft copy when voice is on but a required pack is not ready. `tts` needs both TTS packs (ADR-0063). */
export function voicePackNeedsAttention(
  enabled: boolean,
  tts: boolean,
  download: VoiceDownload,
): { pack: VoicePackKey; status: VoicePackDownload['status'] } | null {
  if (!enabled) return null
  const stt = download.sensevoice.status
  if (stt === 'absent' || stt === 'error') {
    return { pack: 'sensevoice', status: stt }
  }
  if (!tts) return null
  const cantonese = download.ttsCantonese.status
  if (cantonese === 'absent' || cantonese === 'error') {
    return { pack: 'ttsCantonese', status: cantonese }
  }
  const english = download.ttsEnglish.status
  if (english === 'absent' || english === 'error') {
    return { pack: 'ttsEnglish', status: english }
  }
  return null
}
