import { describe, expect, it } from 'vitest'
import en from './locales/en.json'
import zhHK from './locales/zh-HK.json'

/**
 * i18n parity guard: every key in one locale must exist in the other.
 * The plan promised this test when zh-HK landed (118/118 keys); T5 added
 * ~50 more keys per locale — this is what stops them drifting apart.
 */

function flatten(obj: Record<string, unknown>, prefix = ''): string[] {
  return Object.entries(obj).flatMap(([k, v]) =>
    typeof v === 'object' && v !== null
      ? flatten(v as Record<string, unknown>, `${prefix}${k}.`)
      : [`${prefix}${k}`],
  )
}

describe('i18n locale parity', () => {
  const enKeys = flatten(en as Record<string, unknown>)
  const zhKeys = flatten(zhHK as Record<string, unknown>)

  it('zh-HK has every en key', () => {
    const missing = enKeys.filter((k) => !zhKeys.includes(k))
    expect(missing).toEqual([])
  })

  it('en has every zh-HK key', () => {
    const extra = zhKeys.filter((k) => !enKeys.includes(k))
    expect(extra).toEqual([])
  })

  it('no empty translations', () => {
    const empty: string[] = []
    const check = (a: Record<string, unknown>, b: Record<string, unknown>, path = '') => {
      for (const [k, v] of Object.entries(a)) {
        const p = path + k
        if (typeof v === 'object' && v !== null) {
          check(v as Record<string, unknown>, (b[k] ?? {}) as Record<string, unknown>, `${p}.`)
        } else if (typeof b[k] !== 'string' || (b[k] as string).trim() === '') {
          empty.push(p)
        }
      }
    }
    check(en as Record<string, unknown>, zhHK as Record<string, unknown>)
    expect(empty).toEqual([])
  })
})
