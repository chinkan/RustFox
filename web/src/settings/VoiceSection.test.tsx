import type { ReactNode } from 'react'
import { describe, expect, it, vi, beforeEach } from 'vitest'
import { render, screen, fireEvent, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import '../i18n'
import type { Api } from '../api/client'
import type { SettingsPatchResult, VoiceSettings } from '../api/types'
import { VoiceSection } from './VoiceSection'
import { fakeApi } from '../api/fake'

function wrap(ui: ReactNode) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } })
  return render(<QueryClientProvider client={qc}>{ui}</QueryClientProvider>)
}

const baseVoice: VoiceSettings = {
  enabled: true,
  languages: ['yue', 'en'],
  tts: false,
  download: {
    sensevoice: { status: 'ready', message: null },
    ttsCantonese: { status: 'downloading', message: 'fetching vits' },
  },
}

describe('VoiceSection', () => {
  let patchSettings: ReturnType<typeof vi.fn>

  beforeEach(() => {
    patchSettings = vi.fn(async () => ({ updated: ['voiceEnabled'], restartRequired: [] }))
  })

  function api(over: Partial<Api> = {}): Api {
    return fakeApi({
      patchSettings: patchSettings as (patch: import('../api/types').SettingsPatch) => Promise<SettingsPatchResult>,
      ...over,
    })
  }

  it('shows enable, languages, tts, and read-only download status', () => {
    wrap(<VoiceSection api={api()} voice={baseVoice} />)
    expect(screen.getByRole('heading', { name: /Voice \(Telegram\)/i })).toBeTruthy()
    const enable = screen.getByRole('checkbox', { name: /Enable voice pipeline/i }) as HTMLInputElement
    expect(enable.checked).toBe(true)
    const tts = screen.getByRole('checkbox', { name: /Spoken replies/i }) as HTMLInputElement
    expect(tts.checked).toBe(false)
    expect(screen.getByText('ready')).toBeTruthy()
    expect(screen.getByText('downloading')).toBeTruthy()
    expect(screen.getByText('fetching vits')).toBeTruthy()
    expect((screen.getByRole('checkbox', { name: /Cantonese \(yue\)/i }) as HTMLInputElement).checked).toBe(true)
    expect((screen.getByRole('checkbox', { name: /English \(en\)/i }) as HTMLInputElement).checked).toBe(true)
  })

  it('reflects default enable on when voice is omitted from GET', () => {
    wrap(<VoiceSection api={api()} voice={undefined} />)
    const enable = screen.getByRole('checkbox', { name: /Enable voice pipeline/i }) as HTMLInputElement
    expect(enable.checked).toBe(true)
    expect((screen.getByRole('checkbox', { name: /Cantonese \(yue\)/i }) as HTMLInputElement).checked).toBe(true)
    expect((screen.getByRole('checkbox', { name: /English \(en\)/i }) as HTMLInputElement).checked).toBe(true)
  })

  it('disables save and shows validation when all languages are unchecked', () => {
    wrap(<VoiceSection api={api()} voice={baseVoice} />)
    const save = screen.getByRole('button', { name: /^Save$/i })
    expect((save as HTMLButtonElement).disabled).toBe(true)
    fireEvent.click(screen.getByRole('checkbox', { name: /Cantonese \(yue\)/i }))
    fireEvent.click(screen.getByRole('checkbox', { name: /English \(en\)/i }))
    expect(screen.getByText(/Choose at least one language/i)).toBeTruthy()
    expect((save as HTMLButtonElement).disabled).toBe(true)
    expect(patchSettings).not.toHaveBeenCalled()
  })

  it('PATCHes voice whitelist fields on save', async () => {
    wrap(<VoiceSection api={api()} voice={baseVoice} />)
    fireEvent.click(screen.getByRole('checkbox', { name: /Spoken replies/i }))
    const save = screen.getByRole('button', { name: /^Save$/i })
    expect((save as HTMLButtonElement).disabled).toBe(false)
    fireEvent.click(save)
    await waitFor(() => {
      expect(patchSettings).toHaveBeenCalledWith({
        voiceEnabled: true,
        voiceLanguages: ['yue', 'en'],
        voiceTts: true,
      })
    })
  })

  it('shows fail-soft warn when enabled and STT pack is absent', () => {
    wrap(
      <VoiceSection
        api={api()}
        voice={{
          ...baseVoice,
          download: {
            sensevoice: { status: 'absent', message: 'run save to fetch' },
            ttsCantonese: { status: 'ready', message: null },
          },
        }}
      />,
    )
    expect(screen.getByText(/Voice notes may fail softly/i)).toBeTruthy()
    expect(screen.getByText('run save to fetch')).toBeTruthy()
  })
})
