import i18n from 'i18next'
import { initReactI18next } from 'react-i18next'
import en from './locales/en.json'
import zhHK from './locales/zh-HK.json'

/**
 * i18n scaffold (plan grill 8b). en is the fallback; zh-HK (Cantonese)
 * ships with the same key set. Browser language wins, then persisted
 * choice, then English.
 */

export const LANGS = ['en', 'zh-HK'] as const
export type Lang = (typeof LANGS)[number]

const STORAGE_KEY = 'rustfox.portal.lang'

function detectLang(): Lang {
  const saved = typeof localStorage !== 'undefined' ? localStorage.getItem(STORAGE_KEY) : null
  if (saved === 'en' || saved === 'zh-HK') return saved
  const nav = typeof navigator !== 'undefined' ? navigator.language : 'en'
  return nav.startsWith('zh') ? 'zh-HK' : 'en'
}

export function setLang(lang: Lang) {
  void i18n.changeLanguage(lang)
  try {
    localStorage.setItem(STORAGE_KEY, lang)
  } catch {
    /* storage unavailable */
  }
}

void i18n.use(initReactI18next).init({
  resources: {
    en: { translation: en },
    'zh-HK': { translation: zhHK },
  },
  lng: detectLang(),
  fallbackLng: 'en',
  interpolation: { escapeValue: false }, // React already escapes
})

export default i18n
