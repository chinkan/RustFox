import { useEffect, useState, useSyncExternalStore } from 'react'
import { useTranslation } from 'react-i18next'
import { officeMotionEnabled } from './seats'

/** Same persistence style as `rustfox.portal.lang` — this browser only. */
export const OFFICE_PREFS_KEY = 'rustfox.portal.office'

export interface OfficePrefs {
  reduceMotion: boolean
  disableOfficeAnimations: boolean
}

const DEFAULT_PREFS: OfficePrefs = {
  reduceMotion: false,
  disableOfficeAnimations: false,
}

const listeners = new Set<() => void>()
let snapshot: OfficePrefs = loadOfficePrefs()

function loadOfficePrefs(): OfficePrefs {
  if (typeof localStorage === 'undefined') return { ...DEFAULT_PREFS }
  try {
    const raw = localStorage.getItem(OFFICE_PREFS_KEY)
    if (!raw) return { ...DEFAULT_PREFS }
    const parsed: unknown = JSON.parse(raw)
    if (!parsed || typeof parsed !== 'object') return { ...DEFAULT_PREFS }
    const rec = parsed as Record<string, unknown>
    return {
      reduceMotion: rec.reduceMotion === true,
      disableOfficeAnimations: rec.disableOfficeAnimations === true,
    }
  } catch {
    return { ...DEFAULT_PREFS }
  }
}

function emit() {
  for (const listener of listeners) listener()
}

export function readOfficePrefs(): OfficePrefs {
  return loadOfficePrefs()
}

export function writeOfficePrefs(next: OfficePrefs) {
  snapshot = {
    reduceMotion: next.reduceMotion === true,
    disableOfficeAnimations: next.disableOfficeAnimations === true,
  }
  try {
    localStorage.setItem(OFFICE_PREFS_KEY, JSON.stringify(snapshot))
  } catch {
    /* storage unavailable */
  }
  emit()
}

export function subscribeOfficePrefs(listener: () => void) {
  listeners.add(listener)
  return () => listeners.delete(listener)
}

export function __resetOfficePrefsForTests() {
  snapshot = loadOfficePrefs()
  emit()
}

function getSnapshot(): OfficePrefs {
  return snapshot
}

export function useOfficePrefs(): OfficePrefs {
  return useSyncExternalStore(subscribeOfficePrefs, getSnapshot, () => DEFAULT_PREFS)
}

export function systemPrefersReducedMotion(): boolean {
  if (typeof window === 'undefined' || typeof window.matchMedia !== 'function') return false
  try {
    return window.matchMedia('(prefers-reduced-motion: reduce)').matches
  } catch {
    return false
  }
}

export function useOfficeMotion(): boolean {
  const prefs = useOfficePrefs()
  const [prefers, setPrefers] = useState(systemPrefersReducedMotion)

  useEffect(() => {
    if (typeof window.matchMedia !== 'function') return
    let mq: MediaQueryList
    try {
      mq = window.matchMedia('(prefers-reduced-motion: reduce)')
    } catch {
      return
    }
    const apply = () => setPrefers(mq.matches)
    apply()
    mq.addEventListener('change', apply)
    return () => mq.removeEventListener('change', apply)
  }, [])

  return officeMotionEnabled({
    reduceMotion: prefs.reduceMotion,
    disableOfficeAnimations: prefs.disableOfficeAnimations,
    prefersReducedMotion: prefers,
  })
}

/** Settings → Office. Local display prefs, not the whitelisted config API. */
export function OfficePrefsSection() {
  const { t } = useTranslation()
  const prefs = useOfficePrefs()

  return (
    <section className="section" aria-label={t('settings.officeTitle')}>
      <h2 className="section-title">{t('settings.officeTitle')}</h2>
      <div className="card">
        <p className="office-pref-hint" style={{ margin: '0 0 4px' }}>
          {t('settings.officeHint')}
        </p>
        <div className="office-pref">
          <label htmlFor="settings-reduce-motion">
            <input
              id="settings-reduce-motion"
              type="checkbox"
              checked={prefs.reduceMotion}
              onChange={(e) => writeOfficePrefs({ ...prefs, reduceMotion: e.target.checked })}
            />
            {t('settings.reduceMotion')}
          </label>
          <p className="office-pref-hint">{t('settings.reduceMotionHint')}</p>
        </div>
        <div className="office-pref">
          <label htmlFor="settings-disable-office-anim">
            <input
              id="settings-disable-office-anim"
              type="checkbox"
              checked={prefs.disableOfficeAnimations}
              onChange={(e) =>
                writeOfficePrefs({ ...prefs, disableOfficeAnimations: e.target.checked })
              }
            />
            {t('settings.disableOfficeAnimations')}
          </label>
          <p className="office-pref-hint">{t('settings.disableOfficeAnimationsHint')}</p>
        </div>
      </div>
    </section>
  )
}
