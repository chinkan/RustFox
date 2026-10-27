import { beforeEach, describe, expect, it } from 'vitest'
import { renderHook } from '@testing-library/react'
import {
  __resetOfficePrefsForTests,
  OFFICE_PREFS_KEY,
  readOfficePrefs,
  useOfficeMotion,
  writeOfficePrefs,
} from './officePrefs'

beforeEach(() => {
  localStorage.clear()
  __resetOfficePrefsForTests()
})

describe('office prefs', () => {
  it('defaults to animations on', () => {
    expect(readOfficePrefs()).toEqual({
      reduceMotion: false,
      disableOfficeAnimations: false,
    })
  })

  it('round-trips both settings through localStorage', () => {
    writeOfficePrefs({ reduceMotion: true, disableOfficeAnimations: false })
    expect(localStorage.getItem(OFFICE_PREFS_KEY)).toContain('"reduceMotion":true')
    expect(readOfficePrefs()).toEqual({
      reduceMotion: true,
      disableOfficeAnimations: false,
    })
    writeOfficePrefs({ reduceMotion: false, disableOfficeAnimations: true })
    expect(readOfficePrefs().disableOfficeAnimations).toBe(true)
    expect(readOfficePrefs().reduceMotion).toBe(false)
  })

  it('ignores corrupt storage', () => {
    localStorage.setItem(OFFICE_PREFS_KEY, '{not json')
    __resetOfficePrefsForTests()
    expect(readOfficePrefs()).toEqual({
      reduceMotion: false,
      disableOfficeAnimations: false,
    })
  })
})

describe('useOfficeMotion', () => {
  it('follows prefers-reduced-motion', () => {
    const original = window.matchMedia
    window.matchMedia = ((query: string) =>
      ({
        matches: query.includes('prefers-reduced-motion'),
        media: query,
        addEventListener: () => undefined,
        removeEventListener: () => undefined,
        dispatchEvent: () => false,
        onchange: null,
        addListener: () => undefined,
        removeListener: () => undefined,
      }) as MediaQueryList)
    try {
      const { result } = renderHook(() => useOfficeMotion())
      expect(result.current).toBe(false)
    } finally {
      window.matchMedia = original
    }
  })

  it('turns motion off when Disable office animations is stored', () => {
    writeOfficePrefs({ reduceMotion: false, disableOfficeAnimations: true })
    const { result } = renderHook(() => useOfficeMotion())
    expect(result.current).toBe(false)
  })
})
