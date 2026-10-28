import type { Agent, AgentStatus } from '../api/types'

/** Soft cap for desks drawn in the office (ADR-0050). */
export const OFFICE_DESK_CAP = 12

/** Poll while `/agents` is visible. Inside the 2–5s ADR window. */
export const AGENTS_POLL_MS = 4000

export interface SeatView {
  /** Pose to draw. Never `running` without a live-run signal. */
  pose: AgentStatus
  /** Definition-only seat: no live run (glossary). */
  definitionOnly: boolean
}

/**
 * Honest seat presentation for ADR-0050.
 *
 * Prefer `definition_only` / `live_signal` when the seat←run payload includes
 * them (snake_case, PR #312). A row is definition-only when
 * `definition_only === true` or `live_signal === false`. Otherwise trust
 * `status` (`idle` | `running`, and `error` if it ever appears).
 *
 * If a payload omits both fields, only the main seat (`id: "main"` or
 * `platform: "telegram+web"`) trusts `status`. Other rows stay
 * definition-only and never pose `running`.
 */
export function seatView(agent: Agent): SeatView {
  const flagged =
    typeof agent.definition_only === 'boolean' || typeof agent.live_signal === 'boolean'
  if (flagged) {
    const definitionOnly = agent.definition_only === true || agent.live_signal === false
    if (definitionOnly) {
      return { pose: poseWithoutInventedRun(agent.status), definitionOnly: true }
    }
    return { pose: agent.status, definitionOnly: false }
  }
  if (agent.id === 'main' || agent.platform === 'telegram+web') {
    return { pose: agent.status, definitionOnly: false }
  }
  return { pose: poseWithoutInventedRun(agent.status), definitionOnly: true }
}

function poseWithoutInventedRun(status: AgentStatus): AgentStatus {
  return status === 'error' ? 'error' : 'idle'
}

export function visibleDeskSlice<T>(
  items: readonly T[],
  cap = OFFICE_DESK_CAP,
): { visible: T[]; overflow: number } {
  if (items.length <= cap) return { visible: [...items], overflow: 0 }
  return { visible: items.slice(0, cap), overflow: items.length - cap }
}

export function officeMotionEnabled(opts: {
  reduceMotion: boolean
  disableOfficeAnimations: boolean
  prefersReducedMotion: boolean
}): boolean {
  return !opts.reduceMotion && !opts.disableOfficeAnimations && !opts.prefersReducedMotion
}

/** `false` pauses the poll (hidden document or unmounted page). */
export function agentsPollInterval(visible: boolean): number | false {
  return visible ? AGENTS_POLL_MS : false
}

export const FOX_COATS = ['orange', 'red', 'cream', 'silver', 'dark'] as const
export const FOX_ACCESSORIES = ['none', 'scarf', 'glasses', 'bow', 'headset'] as const
export const FOX_PROPS = ['plant', 'mug', 'lamp', 'papers'] as const

export type FoxCoat = (typeof FOX_COATS)[number]
export type FoxAccessory = (typeof FOX_ACCESSORIES)[number]
export type FoxProp = (typeof FOX_PROPS)[number]

export interface FoxVariant {
  coat: FoxCoat
  accessory: FoxAccessory
  deskProp: FoxProp
}

/** FNV-1a. Same name always picks the same fox. */
export function hashName(name: string): number {
  let h = 2166136261
  for (const ch of name) {
    h ^= ch.charCodeAt(0)
    h = Math.imul(h, 16777619)
  }
  return h >>> 0
}

export function foxVariant(name: string): FoxVariant {
  const h = hashName(name)
  return {
    coat: FOX_COATS[h % FOX_COATS.length],
    accessory: FOX_ACCESSORIES[(h >>> 8) % FOX_ACCESSORIES.length],
    deskProp: FOX_PROPS[(h >>> 16) % FOX_PROPS.length],
  }
}

/**
 * Idle roam is not a status. Only a live-signal seat that is idle walks
 * the floor, and only while office motion is allowed.
 */
export function shouldRoam(view: SeatView, motion: boolean): boolean {
  return motion && view.pose === 'idle' && !view.definitionOnly
}

/** Stagger roam loops so foxes do not stack. */
export function roamDelaySeconds(name: string): number {
  return (hashName(name) % 700) / 100
}
