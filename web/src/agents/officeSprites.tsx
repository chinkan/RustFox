import type { ReactNode } from 'react'

/**
 * Usual-office décor, hand-authored on the FoxSprite pixel grid.
 *
 * Pixel anchor (match web/src/agents/FoxSprite.tsx):
 * - 1 SVG unit = 1 pixel. Seat sprites are a 34×32 grid drawn at 96×90 CSS px.
 * - Display scale is 96/34 so a décor pixel is the same size as a fox pixel.
 * - Palette is the seat fills only: ink #1a1206, cream #f4efe4, white #f7f8fb,
 *   desk #c9844a, desk-dark #6e4424, leaf #3fb950, blue #58a6ff, red #f85149,
 *   lamp #ffd866, metal #d5dbe3, fur accent. No extra hexes.
 * - 1px ink or desk-dark outline, 2–3 flat tones, no gradients, no anti-aliasing.
 * - Front view with a 2px top lip. Not isometric.
 *
 * Tile specs: window 32×18, door 16×36, rug 28×12, plant 16×20,
 * shelf 24×32, cabinet 18×32, lamp 14×22, clock 14×14, poster 18×24.
 */

const PX = 96 / 34

function Tile({
  piece,
  width,
  height,
  children,
}: {
  piece: string
  width: number
  height: number
  children: ReactNode
}) {
  return (
    <svg
      className="office-sprite"
      viewBox={`0 0 ${width} ${height}`}
      width={width * PX}
      height={height * PX}
      shapeRendering="crispEdges"
      data-decor-piece={piece}
    >
      {children}
    </svg>
  )
}

export function OfficeDecor() {
  return (
    <div className="office-decor" aria-hidden="true">
      <div className="office-wall" aria-hidden="true">
        <Window />
        <Window />
        <div className="office-wall-end">
          <Poster />
          <Clock />
        </div>
      </div>
      <div className="office-side office-side-left" aria-hidden="true">
        <Door />
        <Rug />
        <Plant />
      </div>
      <div className="office-side office-side-right" aria-hidden="true">
        <Shelf />
        <Cabinet />
        <Lamp />
      </div>
    </div>
  )
}

function Window() {
  return (
    <Tile piece="window" width={32} height={18}>
      <rect className="px-ink" width="32" height="18" />
      <rect className="px-cream" x="1" y="1" width="30" height="14" />
      <rect className="px-scarf" x="2" y="2" width="13" height="5" />
      <rect className="px-scarf" x="17" y="2" width="13" height="5" />
      <rect className="px-scarf" x="2" y="9" width="13" height="5" />
      <rect className="px-scarf" x="17" y="9" width="13" height="5" />
      <rect className="px-white" x="3" y="3" width="3" height="2" />
      <rect className="px-white" x="18" y="3" width="3" height="2" />
      <rect className="px-cream" x="15" y="2" width="2" height="12" />
      <rect className="px-cream" x="2" y="7" width="28" height="2" />
      <rect className="px-desk" x="0" y="15" width="32" height="2" />
      <rect className="px-desk-dark" x="0" y="17" width="32" height="1" />
    </Tile>
  )
}

function Door() {
  return (
    <Tile piece="door" width={16} height={36}>
      <rect className="px-ink" width="16" height="36" />
      <rect className="px-desk-dark" x="1" y="1" width="14" height="2" />
      <rect className="px-desk" x="2" y="4" width="12" height="13" />
      <rect className="px-desk-dark" x="3" y="5" width="10" height="1" />
      <rect className="px-desk" x="2" y="19" width="12" height="15" />
      <rect className="px-desk-dark" x="3" y="20" width="10" height="1" />
      <rect className="px-ink" x="2" y="17" width="12" height="2" />
      <rect className="px-lamp" x="11" y="17" width="2" height="2" />
    </Tile>
  )
}

function Rug() {
  return (
    <Tile piece="rug" width={28} height={12}>
      <rect className="px-ink" width="28" height="12" />
      <rect className="px-bow" x="1" y="1" width="4" height="10" />
      <rect className="px-cream" x="5" y="1" width="4" height="10" />
      <rect className="px-scarf" x="9" y="1" width="4" height="10" />
      <rect className="px-cream" x="13" y="1" width="4" height="10" />
      <rect className="px-bow" x="17" y="1" width="4" height="10" />
      <rect className="px-cream" x="21" y="1" width="3" height="10" />
      <rect className="px-scarf" x="24" y="1" width="3" height="10" />
      <rect className="px-desk-dark" x="2" y="4" width="24" height="1" />
      <rect className="px-desk-dark" x="2" y="7" width="24" height="1" />
    </Tile>
  )
}

function Plant() {
  return (
    <Tile piece="plant" width={16} height={20}>
      <rect className="px-leaf" x="2" y="1" width="5" height="6" />
      <rect className="px-leaf" x="8" y="0" width="6" height="5" />
      <rect className="px-leaf" x="5" y="5" width="6" height="5" />
      <rect className="px-ink" x="3" y="2" width="2" height="2" />
      <rect className="px-ink" x="7" y="8" width="2" height="4" />
      <rect className="px-desk-dark" x="4" y="12" width="8" height="2" />
      <rect className="px-desk" x="3" y="14" width="10" height="5" />
      <rect className="px-ink" x="3" y="14" width="10" height="1" />
      <rect className="px-desk-dark" x="4" y="18" width="8" height="2" />
    </Tile>
  )
}

function Shelf() {
  return (
    <Tile piece="shelf" width={24} height={32}>
      <rect className="px-ink" width="24" height="32" />
      <rect className="px-cream" x="1" y="2" width="22" height="29" />
      <rect className="px-desk" x="1" y="1" width="22" height="2" />
      <rect className="px-scarf" x="2" y="4" width="3" height="8" />
      <rect className="px-bow" x="6" y="5" width="3" height="7" />
      <rect className="px-leaf" x="10" y="4" width="3" height="8" />
      <rect className="px-lamp" x="14" y="6" width="3" height="6" />
      <rect className="px-fur" x="18" y="4" width="4" height="8" />
      <rect className="px-desk-dark" x="1" y="12" width="22" height="2" />
      <rect className="px-white" x="3" y="15" width="6" height="7" />
      <rect className="px-ink" x="4" y="16" width="4" height="1" />
      <rect className="px-bezel" x="11" y="15" width="5" height="7" />
      <rect className="px-scarf" x="17" y="16" width="4" height="6" />
      <rect className="px-desk-dark" x="1" y="22" width="22" height="2" />
      <rect className="px-key" x="3" y="25" width="8" height="5" />
      <rect className="px-desk" x="13" y="26" width="8" height="4" />
    </Tile>
  )
}

function Cabinet() {
  return (
    <Tile piece="cabinet" width={18} height={32}>
      <rect className="px-ink" width="18" height="32" />
      <rect className="px-key" x="1" y="3" width="16" height="28" />
      <rect className="px-white" x="1" y="1" width="16" height="2" />
      <rect className="px-ink" x="1" y="11" width="16" height="1" />
      <rect className="px-ink" x="1" y="20" width="16" height="1" />
      <rect className="px-ink" x="7" y="6" width="4" height="2" />
      <rect className="px-ink" x="7" y="15" width="4" height="2" />
      <rect className="px-ink" x="7" y="24" width="4" height="2" />
    </Tile>
  )
}

function Lamp() {
  return (
    <Tile piece="lamp" width={14} height={22}>
      <rect className="px-lamp" x="2" y="0" width="10" height="6" />
      <rect className="px-ink" x="2" y="5" width="10" height="1" />
      <rect className="px-white" x="3" y="1" width="3" height="2" />
      <rect className="px-ink" x="6" y="6" width="2" height="12" />
      <rect className="px-desk-dark" x="2" y="18" width="10" height="3" />
      <rect className="px-ink" x="1" y="20" width="12" height="2" />
    </Tile>
  )
}

function Clock() {
  return (
    <Tile piece="clock" width={14} height={14}>
      <rect className="px-ink" x="2" y="0" width="10" height="14" />
      <rect className="px-ink" x="0" y="2" width="14" height="10" />
      <rect className="px-white" x="3" y="1" width="8" height="12" />
      <rect className="px-white" x="1" y="3" width="12" height="8" />
      <rect className="px-ink" x="6" y="3" width="2" height="5" />
      <rect className="px-ink" x="6" y="6" width="5" height="2" />
    </Tile>
  )
}

function Poster() {
  return (
    <Tile piece="poster" width={18} height={24}>
      <rect className="px-ink" width="18" height="24" />
      <rect className="px-cream" x="1" y="1" width="16" height="22" />
      <rect className="px-fur" x="4" y="3" width="3" height="3" />
      <rect className="px-fur" x="11" y="3" width="3" height="3" />
      <rect className="px-shade" x="5" y="4" width="1" height="1" />
      <rect className="px-shade" x="12" y="4" width="1" height="1" />
      <rect className="px-fur" x="4" y="6" width="10" height="6" />
      <rect className="px-cream" x="7" y="8" width="6" height="3" />
      <rect className="px-ink" x="8" y="8" width="1" height="1" />
      <rect className="px-ink" x="11" y="8" width="1" height="1" />
      <rect className="px-ink" x="10" y="10" width="1" height="1" />
      <rect className="px-fur" x="5" y="12" width="8" height="5" />
      <rect className="px-fur" x="2" y="14" width="4" height="3" />
      <rect className="px-cream" x="1" y="16" width="2" height="2" />
      <rect className="px-desk-dark" x="6" y="17" width="2" height="4" />
      <rect className="px-desk-dark" x="10" y="17" width="2" height="4" />
    </Tile>
  )
}
