/** Desk anchor in room space (≤12 seats, ADR-0050 soft cap). */
export interface IsoDeskSlot {
  index: number
  x: number
  z: number
  rotationY: number
}

const GRID_COLS = 4

/**
 * Simple dollhouse grid: two staggered rows along Z, four columns along X.
 * Indices follow the same even/odd split as PixelOffice (top row = even indices).
 */
export function isoDeskSlots(count: number): IsoDeskSlot[] {
  const n = Math.min(count, 12)
  const slots: IsoDeskSlot[] = []
  for (let i = 0; i < n; i++) {
    const col = i % GRID_COLS
    const row = Math.floor(i / GRID_COLS)
    const x = (col - (GRID_COLS - 1) / 2) * 2.2
    const z = row === 0 ? -1.4 : 1.4
    const rotationY = row === 0 ? 0 : Math.PI
    slots.push({ index: i, x, z, rotationY })
  }
  return slots
}
