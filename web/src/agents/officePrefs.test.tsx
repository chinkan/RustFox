import { beforeEach, describe, expect, it } from 'vitest'
import { renderHook } from '@testing-library/react'
import {
  __resetOfficePrefsForTests,
  effectiveOfficeVisualize,
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
      office_visualize: 'svg',
    })
  })

  it('round-trips settings including office_visualize through localStorage', () => {
    writeOfficePrefs({ reduceMotion: true, disableOfficeAnimations: false, office_visualize: 'svg' })
    expect(localStorage.getItem(OFFICE_PREFS_KEY)).toContain('"reduceMotion":true')
    expect(readOfficePrefs()).toEqual({
      reduceMotion: true,
      disableOfficeAnimations: false,
      office_visualize: 'svg',
    })
    writeOfficePrefs({ reduceMotion: false, disableOfficeAnimations: true, office_visualize: 'webgl' })
    expect(readOfficePrefs().disableOfficeAnimations).toBe(true)
    expect(readOfficePrefs().reduceMotion).toBe(false)
    expect(readOfficePrefs().office_visualize).toBe('webgl')
    expect(localStorage.getItem(OFFICE_PREFS_KEY)).toContain('"office_visualize":"webgl"')
  })

  it('persists office_visualize across reload from storage', () => {
    writeOfficePrefs({ reduceMotion: false, disableOfficeAnimations: false, office_visualize: 'webgl' })
    __resetOfficePrefsForTests()
    expect(readOfficePrefs().office_visualize).toBe('webgl')
  })

  it('ignores corrupt storage', () => {
    localStorage.setItem(OFFICE_PREFS_KEY, '{not json')
    __resetOfficePrefsForTests()
    expect(readOfficePrefs()).toEqual({
      reduceMotion: false,
      disableOfficeAnimations: false,
      office_visualize: 'svg',
    })
  })
})

describe('effectiveOfficeVisualize', () => {
  it('forces svg when office motion is off', () => {
    expect(effectiveOfficeVisualize('webgl', false)).toBe('svg')
    expect(effectiveOfficeVisualize('webgl', true)).toBe('webgl')
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
    writeOfficePrefs({ reduceMotion: false, disableOfficeAnimations: true, office_visualize: 'svg' })
    const { result } = renderHook(() => useOfficeMotion())
    expect(result.current).toBe(false)
  })
})
