import { useCallback, useSyncExternalStore } from 'react'

/** Session-only office renderer (refresh → Pixel default). Not Settings-persisted. */
export const OFFICE_RENDERER_SESSION_KEY = 'rustfox.portal.office.renderer'

export type OfficeRenderer = 'pixel' | '3d'

const listeners = new Set<() => void>()

function readSession(): OfficeRenderer {
  if (typeof sessionStorage === 'undefined') return 'pixel'
  try {
    const raw = sessionStorage.getItem(OFFICE_RENDERER_SESSION_KEY)
    return raw === '3d' ? '3d' : 'pixel'
  } catch {
    return 'pixel'
  }
}

let snapshot: OfficeRenderer = readSession()

function emit() {
  for (const listener of listeners) listener()
}

export function getOfficeRenderer(): OfficeRenderer {
  return snapshot
}

export function setOfficeRenderer(next: OfficeRenderer) {
  snapshot = next === '3d' ? '3d' : 'pixel'
  try {
    if (next === 'pixel') {
      sessionStorage.removeItem(OFFICE_RENDERER_SESSION_KEY)
    } else {
      sessionStorage.setItem(OFFICE_RENDERER_SESSION_KEY, '3d')
    }
  } catch {
    /* sessionStorage unavailable */
  }
  emit()
}

export function subscribeOfficeRenderer(listener: () => void) {
  listeners.add(listener)
  return () => listeners.delete(listener)
}

export function __resetOfficeRendererForTests() {
  snapshot = readSession()
  emit()
}

function getSnapshot(): OfficeRenderer {
  return snapshot
}

export function useOfficeRenderer(): [OfficeRenderer, (next: OfficeRenderer) => void] {
  const renderer = useSyncExternalStore(subscribeOfficeRenderer, getSnapshot, () => 'pixel' as OfficeRenderer)
  const set = useCallback((next: OfficeRenderer) => setOfficeRenderer(next), [])
  return [renderer, set]
}
