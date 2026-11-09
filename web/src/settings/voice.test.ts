import { describe, expect, it } from 'vitest'
import {
  normalizeVoiceSettings,
  sortVoiceLanguages,
  voicePackNeedsAttention,
  voiceSettingsDirty,
} from './voice'

describe('voice settings helpers', () => {
  it('defaults absent GET to enabled true and yue+en', () => {
    expect(normalizeVoiceSettings(undefined)).toEqual({
      enabled: true,
      languages: ['yue', 'en'],
      tts: false,
      download: {
        sensevoice: { status: 'absent', message: null },
        ttsCantonese: { status: 'absent', message: null },
      },
    })
  })

  it('sorts languages in ADR order and drops unknown ids', () => {
    expect(sortVoiceLanguages(['en', 'ko', 'yue', 'nope'])).toEqual(['yue', 'en', 'ko'])
  })

  it('detects dirty state for enable, languages, and tts', () => {
    const server = normalizeVoiceSettings({
      enabled: true,
      languages: ['yue', 'en'],
      tts: false,
      download: {
        sensevoice: { status: 'ready', message: null },
        ttsCantonese: { status: 'absent', message: null },
      },
    })
    expect(voiceSettingsDirty(server, false, server.languages, server.tts)).toBe(true)
    expect(voiceSettingsDirty(server, server.enabled, ['yue'], server.tts)).toBe(true)
    expect(voiceSettingsDirty(server, server.enabled, server.languages, true)).toBe(true)
    expect(voiceSettingsDirty(server, server.enabled, server.languages, server.tts)).toBe(false)
  })

  it('flags STT pack attention when enabled and pack missing', () => {
    const download = {
      sensevoice: { status: 'error' as const, message: 'bin missing' },
      ttsCantonese: { status: 'ready' as const, message: null },
    }
    expect(voicePackNeedsAttention(true, false, download)).toEqual({
      pack: 'sensevoice',
      status: 'error',
    })
    expect(voicePackNeedsAttention(false, false, download)).toBeNull()
  })

  it('flags TTS pack when tts on and pack absent', () => {
    const download = {
      sensevoice: { status: 'ready' as const, message: null },
      ttsCantonese: { status: 'absent' as const, message: null },
    }
    expect(voicePackNeedsAttention(true, true, download)?.pack).toBe('ttsCantonese')
  })
})
