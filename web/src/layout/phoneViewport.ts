import { useSyncExternalStore } from 'react'

/** Covers a typical phone width (375–430 CSS px) and anything narrower. */
export const PHONE_VIEWPORT_MAX_PX = 767

export const PHONE_VIEWPORT_QUERY = `(max-width: ${PHONE_VIEWPORT_MAX_PX}px)`

function phoneMedia(): MediaQueryList | null {
  if (typeof window === 'undefined' || typeof window.matchMedia !== 'function') return null
  return window.matchMedia(PHONE_VIEWPORT_QUERY)
}

function subscribe(onStoreChange: () => void) {
  const media = phoneMedia()
  if (!media) return () => undefined
  media.addEventListener('change', onStoreChange)
  return () => media.removeEventListener('change', onStoreChange)
}

function snapshot() {
  return phoneMedia()?.matches ?? false
}

function serverSnapshot() {
  return false
}

/** True when the portal should use the single-column phone shell. */
export function usePhoneViewport(): boolean {
  return useSyncExternalStore(subscribe, snapshot, serverSnapshot)
}
