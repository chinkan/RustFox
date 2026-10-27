import { describe, expect, it } from 'vitest'
import type { Agent } from '../api/types'
import {
  AGENTS_POLL_MS,
  OFFICE_DESK_CAP,
  agentsPollInterval,
  officeMotionEnabled,
  seatView,
  visibleDeskSlice,
} from './seats'

function agent(over: Partial<Agent> = {}): Agent {
  return {
    id: 'agent:verifier',
    name: 'verifier',
    model: 'openai/gpt-5.5',
    status: 'idle',
    lastActive: null,
    platform: 'subagent',
    live_signal: true,
    definition_only: false,
    ...over,
  }
}

/** Payload from before seat←run fields existed. */
function legacy(over: Partial<Agent> = {}): Agent {
  const row = agent(over)
  delete (row as Partial<Agent>).live_signal
  delete (row as Partial<Agent>).definition_only
  return row
}

describe('seatView honesty (ADR-0050)', () => {
  it('trusts the main agent live status', () => {
    expect(
      seatView(agent({ id: 'main', name: 'rustfox', platform: 'telegram+web', status: 'running' })),
    ).toEqual({ pose: 'running', definitionOnly: false })
    expect(
      seatView(agent({ id: 'main', name: 'rustfox', platform: 'telegram+web', status: 'idle' })),
    ).toEqual({ pose: 'idle', definitionOnly: false })
    expect(
      seatView(agent({ id: 'main', name: 'rustfox', platform: 'telegram+web', status: 'error' })),
    ).toEqual({ pose: 'error', definitionOnly: false })
  })

  it('treats a telegram+web row as the main seat even if the id is unexpected', () => {
    expect(seatView(agent({ id: 'bot', platform: 'telegram+web', status: 'running' }))).toEqual({
      pose: 'running',
      definitionOnly: false,
    })
  })

  it('labels a non-main row with no live-run flag as definition only and does not pose running', () => {
    expect(seatView(legacy({ status: 'idle' }))).toEqual({ pose: 'idle', definitionOnly: true })
    expect(seatView(legacy({ status: 'running' }))).toEqual({ pose: 'idle', definitionOnly: true })
  })

  it('keeps an error pose when the API reports error on a definition-only seat', () => {
    expect(seatView(legacy({ status: 'error' }))).toEqual({ pose: 'error', definitionOnly: true })
  })

  it('trusts status when live_signal is true and the row is not definition_only', () => {
    expect(seatView(agent({ status: 'running', definition_only: false, live_signal: true }))).toEqual({
      pose: 'running',
      definitionOnly: false,
    })
    expect(seatView(agent({ status: 'running', live_signal: true }))).toEqual({
      pose: 'running',
      definitionOnly: false,
    })
    expect(seatView(agent({ status: 'running', definition_only: false }))).toEqual({
      pose: 'running',
      definitionOnly: false,
    })
    expect(seatView(agent({ status: 'idle', live_signal: true, definition_only: false }))).toEqual({
      pose: 'idle',
      definitionOnly: false,
    })
    expect(seatView(agent({ status: 'error', live_signal: true, definition_only: false }))).toEqual({
      pose: 'error',
      definitionOnly: false,
    })
  })

  it('labels definition-only when definition_only is true or live_signal is false', () => {
    expect(seatView(agent({ status: 'running', definition_only: true, live_signal: true }))).toEqual({
      pose: 'idle',
      definitionOnly: true,
    })
    expect(seatView(agent({ status: 'running', definition_only: false, live_signal: false }))).toEqual({
      pose: 'idle',
      definitionOnly: true,
    })
    expect(seatView(agent({ status: 'running', live_signal: false }))).toEqual({
      pose: 'idle',
      definitionOnly: true,
    })
    expect(seatView(agent({ status: 'error', definition_only: true }))).toEqual({
      pose: 'error',
      definitionOnly: true,
    })
  })
})

describe('visibleDeskSlice', () => {
  it('shows every desk at or under the soft cap', () => {
    const items = Array.from({ length: OFFICE_DESK_CAP }, (_, i) => i)
    expect(visibleDeskSlice(items)).toEqual({ visible: items, overflow: 0 })
    expect(OFFICE_DESK_CAP).toBe(12)
  })

  it('caps visible desks and reports the remainder', () => {
    const items = Array.from({ length: 14 }, (_, i) => i)
    const slice = visibleDeskSlice(items)
    expect(slice.visible).toEqual(items.slice(0, 12))
    expect(slice.overflow).toBe(2)
  })

  it('returns an empty office for no rows', () => {
    expect(visibleDeskSlice([])).toEqual({ visible: [], overflow: 0 })
  })
})

describe('officeMotionEnabled', () => {
  const base = { reduceMotion: false, disableOfficeAnimations: false, prefersReducedMotion: false }

  it('animates by default', () => {
    expect(officeMotionEnabled(base)).toBe(true)
  })

  it('stops pose loops when any reduce-motion control is on', () => {
    expect(officeMotionEnabled({ ...base, reduceMotion: true })).toBe(false)
    expect(officeMotionEnabled({ ...base, disableOfficeAnimations: true })).toBe(false)
    expect(officeMotionEnabled({ ...base, prefersReducedMotion: true })).toBe(false)
  })
})

describe('agentsPollInterval', () => {
  it('polls every 2–5s while the page is visible', () => {
    expect(agentsPollInterval(true)).toBe(AGENTS_POLL_MS)
    expect(AGENTS_POLL_MS).toBeGreaterThanOrEqual(2000)
    expect(AGENTS_POLL_MS).toBeLessThanOrEqual(5000)
  })

  it('pauses while the document is hidden', () => {
    expect(agentsPollInterval(false)).toBe(false)
  })
})
