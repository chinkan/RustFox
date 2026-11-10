import { describe, expect, it } from 'vitest'
import en from '../locales/en.json'
import zhHK from '../locales/zh-HK.json'
import type { VoiceDownload } from '../api/types'
import {
  normalizeVoiceSettings,
  sortVoiceLanguages,
  voiceFromSettings,
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
        ttsEnglish: { status: 'absent', message: null },
      },
    })
  })

  it('fills ttsEnglish when GET omits download or that pack', () => {
    const omitted = normalizeVoiceSettings({
      enabled: true,
      languages: ['yue'],
      tts: false,
      download: undefined,
    })
    expect(omitted.download.ttsEnglish).toEqual({ status: 'absent', message: null })

    const partial = normalizeVoiceSettings({
      enabled: true,
      languages: ['yue', 'en'],
      tts: true,
      download: {
        sensevoice: { status: 'ready', message: 'ok' },
        ttsCantonese: { status: 'downloading', message: 'fetching' },
      },
    })
    expect(partial.download).toEqual({
      sensevoice: { status: 'ready', message: 'ok' },
      ttsCantonese: { status: 'downloading', message: 'fetching' },
      ttsEnglish: { status: 'absent', message: null },
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
        ttsEnglish: { status: 'absent', message: null },
      },
    })
    expect(voiceSettingsDirty(server, false, server.languages, server.tts)).toBe(true)
    expect(voiceSettingsDirty(server, server.enabled, ['yue'], server.tts)).toBe(true)
    expect(voiceSettingsDirty(server, server.enabled, server.languages, true)).toBe(true)
    expect(voiceSettingsDirty(server, server.enabled, server.languages, server.tts)).toBe(false)
  })

  it('flags STT pack attention when enabled and pack missing', () => {
    const download: VoiceDownload = {
      sensevoice: { status: 'error', message: 'bin missing' },
      ttsCantonese: { status: 'ready', message: null },
      ttsEnglish: { status: 'ready', message: null },
    }
    expect(voicePackNeedsAttention(true, false, download)).toEqual({
      pack: 'sensevoice',
      status: 'error',
    })
    expect(voicePackNeedsAttention(false, false, download)).toBeNull()
  })

  it('flags TTS pack when tts on and pack absent', () => {
    const download: VoiceDownload = {
      sensevoice: { status: 'ready', message: null },
      ttsCantonese: { status: 'absent', message: null },
      ttsEnglish: { status: 'ready', message: null },
    }
    expect(voicePackNeedsAttention(true, true, download)?.pack).toBe('ttsCantonese')
  })

  it('flags English TTS when tts is on and that pack is absent or error', () => {
    const absent: VoiceDownload = {
      sensevoice: { status: 'ready', message: null },
      ttsCantonese: { status: 'ready', message: null },
      ttsEnglish: { status: 'absent', message: null },
    }
    expect(voicePackNeedsAttention(true, true, absent)).toEqual({
      pack: 'ttsEnglish',
      status: 'absent',
    })

    const error: VoiceDownload = {
      ...absent,
      ttsEnglish: { status: 'error', message: 'unpack failed' },
    }
    expect(voicePackNeedsAttention(true, true, error)).toEqual({
      pack: 'ttsEnglish',
      status: 'error',
    })
    expect(voicePackNeedsAttention(true, false, error)).toBeNull()
    expect(voicePackNeedsAttention(false, true, error)).toBeNull()
  })

  it('does not flag English TTS while the pack is downloading or ready', () => {
    const downloading: VoiceDownload = {
      sensevoice: { status: 'ready', message: null },
      ttsCantonese: { status: 'ready', message: null },
      ttsEnglish: { status: 'downloading', message: null },
    }
    const ready: VoiceDownload = {
      ...downloading,
      ttsEnglish: { status: 'ready', message: null },
    }
    expect(voicePackNeedsAttention(true, true, downloading)).toBeNull()
    expect(voicePackNeedsAttention(true, true, ready)).toBeNull()
  })

  it('maps the live GET shape and ignores a nested voice object', () => {
    const live = {
      editable: {
        voiceEnabled: true,
        voiceLanguages: ['ko', 'en', 'en'],
        voiceTts: true,
      },
      voiceDownload: {
        sensevoice: { status: 'ready' as const, message: 'Ready.' },
        ttsCantonese: { status: 'ready' as const, message: 'Ready.' },
        ttsEnglish: { status: 'error' as const, message: 'unpack failed' },
      },
      voice: {
        enabled: false,
        languages: ['ja'],
        tts: false,
        download: {
          sensevoice: { status: 'absent' as const, message: null },
          ttsCantonese: { status: 'absent' as const, message: null },
          ttsEnglish: { status: 'absent' as const, message: null },
        },
      },
    }
    expect(voiceFromSettings(live)).toEqual({
      enabled: true,
      languages: ['en', 'ko'],
      tts: true,
      download: {
        sensevoice: { status: 'ready', message: 'Ready.' },
        ttsCantonese: { status: 'ready', message: 'Ready.' },
        ttsEnglish: { status: 'error', message: 'unpack failed' },
      },
    })
    expect(voicePackNeedsAttention(true, true, voiceFromSettings(live).download)?.pack).toBe('ttsEnglish')
  })

  it('defaults a live GET that omits voice fields or ttsEnglish', () => {
    expect(voiceFromSettings({ editable: {} })).toEqual(normalizeVoiceSettings(undefined))
    const partial = voiceFromSettings({
      editable: { voiceEnabled: false, voiceLanguages: [], voiceTts: true },
      voiceDownload: {
        sensevoice: { status: 'ready', message: null },
        ttsCantonese: { status: 'downloading', message: 'fetching' },
      },
    })
    expect(partial.enabled).toBe(false)
    expect(partial.tts).toBe(true)
    expect(partial.languages).toEqual(['yue', 'en'])
    expect(partial.download.ttsEnglish).toEqual({ status: 'absent', message: null })
  })

  it('names English and Cantonese spoken replies in both locales', () => {
    expect(en.settings.voicePackTtsEnglish).toBe('English TTS')
    expect(zhHK.settings.voicePackTtsEnglish).toBe('英文 TTS')
    expect(en.settings.voiceHint).toMatch(/Cantonese and English/)
    expect(en.settings.voiceTts).toMatch(/Cantonese and English/)
    expect(zhHK.settings.voiceHint).toMatch(/廣東話同英文/)
    expect(zhHK.settings.voiceTts).toMatch(/廣東話同英文/)
    expect(en.settings.voiceHint).not.toMatch(/Cantonese TTS for/)
    expect(zhHK.settings.voiceTts).not.toBe('語音回覆（廣東話 TTS）')
  })
})
