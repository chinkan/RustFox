import { describe, expect, it } from 'vitest'
import { CRON_5_OR_6_FIELD } from './types'

describe('CRON_5_OR_6_FIELD (ADR-0021)', () => {
  it('accepts 5 and 6 fields', () => {
    expect(CRON_5_OR_6_FIELD.test('0 9 * * *')).toBe(true)
    expect(CRON_5_OR_6_FIELD.test('0 0 9 * * *')).toBe(true)
  })
  it('rejects 4 and 7 fields', () => {
    expect(CRON_5_OR_6_FIELD.test('9 * * *')).toBe(false)
    expect(CRON_5_OR_6_FIELD.test('0 0 9 * * * 2026')).toBe(false)
  })
})
