import type { AgentStatus } from '../api/types'
import type { FoxAccessory, FoxProp, FoxVariant } from './seats'

/**
 * Composable seat. Desk, monitor, fox, and desk décor are separate props.
 * Every prop in one seat shares the same `dir`. A seat never mixes dirs.
 */
export type SeatDir = 'down' | 'left' | 'right' | 'up'
export type FoxFacing = SeatDir

export function FoxSprite({
  pose,
  variant,
  mode = 'seat',
  facing = 'down',
}: {
  pose: AgentStatus
  variant: FoxVariant
  mode?: 'seat' | 'walk' | 'desk'
  /** Shared direction for every prop in this seat. */
  facing?: FoxFacing
}) {
  const dir: SeatDir = mode === 'walk' ? walkDir(facing) : facing
  const shown = mode === 'walk' ? 'walk' : pose
  const furniture = mode !== 'walk'
  const showFox = mode !== 'desk'
  const desk = furniture ? <Desk dir={dir} prop={variant.deskProp} pose={pose} /> : null
  const monitor = furniture ? <Monitor dir={dir} pose={pose} /> : null
  const fox = showFox ? (
    shown === 'walk' ? (
      <WalkFox dir={dir} accessory={variant.accessory} />
    ) : (
      <Fox dir={dir} pose={pose} accessory={variant.accessory} />
    )
  ) : null
  // Up: monitor is on the far side of the desk, fox in front.
  // Other dirs: monitor is in front of the fox, standing on the desk.
  const ordered = dir === 'up' ? (
    <>
      {monitor}
      {desk}
      {fox}
    </>
  ) : (
    <>
      {desk}
      {fox}
      {monitor}
    </>
  )
  return (
    <svg
      className={`seat-sprite pose-${shown} coat-${variant.coat}`}
      viewBox={mode === 'walk' ? '0 0 34 32' : '0 0 48 40'}
      width={mode === 'walk' ? 68 : 96}
      height={mode === 'walk' ? 64 : 80}
      shapeRendering="crispEdges"
      aria-hidden="true"
      data-character={showFox ? 'fox' : undefined}
      data-coat={variant.coat}
      data-accessory={variant.accessory}
      data-desk-prop={variant.deskProp}
      data-facing={dir}
      data-dir={dir}
    >
      {ordered}
    </svg>
  )
}

function walkDir(facing: FoxFacing): 'left' | 'right' {
  return facing === 'left' ? 'left' : 'right'
}

function Desk({ dir, prop, pose }: { dir: SeatDir; prop: FoxProp; pose: AgentStatus }) {
  const top = deskTop(dir)
  return (
    <g data-seat-prop="desk" data-fox-part="desk" data-dir={dir}>
      <rect className="px-desk" data-fox-part="desk-top" x={top.x} y={top.y} width={top.w} height={top.h} />
      <rect className="px-desk-dark" x={top.x} y={top.y + top.h} width={top.w} height="2" />
      <rect className="px-desk-dark" x={top.x + 2} y={top.y + top.h} width="3" height="6" />
      <rect className="px-desk-dark" x={top.x + top.w - 5} y={top.y + top.h} width="3" height="6" />
      <Decor kind={prop} dir={dir} />
      {pose === 'running' ? <Keyboard dir={dir} /> : null}
    </g>
  )
}

function deskTop(dir: SeatDir): { x: number; y: number; w: number; h: number } {
  if (dir === 'down') return { x: 1, y: 26, w: 46, h: 6 }
  if (dir === 'up') return { x: 1, y: 12, w: 46, h: 6 }
  if (dir === 'left') return { x: 18, y: 18, w: 28, h: 8 }
  return { x: 2, y: 18, w: 28, h: 8 }
}

function Monitor({ dir, pose }: { dir: SeatDir; pose: AgentStatus }) {
  const screen = pose === 'running' ? 'px-screen-run' : pose === 'error' ? 'px-screen-err' : 'px-screen-idle'
  const box = monitorBox(dir)
  return (
    <g data-seat-prop="monitor" data-fox-part="monitor-rig" data-dir={dir}>
      <rect className="px-bezel" data-fox-part="monitor" x={box.x} y={box.y} width={box.w} height={box.h} />
      <rect className={screen} x={box.x + 2} y={box.y + 2} width={box.w - 4} height={box.h - 4} />
      {pose === 'error' ? (
        <g className="px-alert">
          <rect className="px-ink" x={box.x + 5} y={box.y + 3} width="2" height="2" />
          <rect className="px-ink" x={box.x + 5} y={box.y + 6} width="2" height="1" />
        </g>
      ) : null}
      <rect
        className="px-bezel"
        data-fox-part="monitor-stand"
        x={box.standX}
        y={box.standY}
        width="6"
        height={box.standH}
      />
    </g>
  )
}

function monitorBox(dir: SeatDir): {
  x: number
  y: number
  w: number
  h: number
  standX: number
  standY: number
  standH: number
} {
  if (dir === 'down') return { x: 16, y: 14, w: 16, h: 12, standX: 21, standY: 26, standH: 4 }
  if (dir === 'up') return { x: 16, y: 0, w: 16, h: 8, standX: 21, standY: 8, standH: 4 }
  if (dir === 'left') return { x: 0, y: 8, w: 12, h: 10, standX: 12, standY: 18, standH: 4 }
  return { x: 36, y: 8, w: 12, h: 10, standX: 30, standY: 18, standH: 4 }
}

function Keyboard({ dir }: { dir: SeatDir }) {
  const key = keyBox(dir)
  return (
    <rect
      className="px-key"
      data-seat-prop="keyboard"
      data-fox-part="keyboard"
      data-dir={dir}
      x={key.x}
      y={key.y}
      width={key.w}
      height="3"
    />
  )
}

function keyBox(dir: SeatDir): { x: number; y: number; w: number } {
  if (dir === 'down') return { x: 32, y: 28, w: 10 }
  if (dir === 'up') return { x: 34, y: 14, w: 10 }
  if (dir === 'left') return { x: 20, y: 20, w: 8 }
  return { x: 20, y: 20, w: 8 }
}

function Decor({ kind, dir }: { kind: FoxProp; dir: SeatDir }) {
  const o = decorOrigin(dir)
  if (kind === 'plant') {
    return (
      <g data-seat-prop="plant" data-fox-part="prop" data-prop="plant" data-dir={dir}>
        <rect className="px-leaf" x={o.x} y={o.y - 6} width="4" height="4" />
        <rect className="px-leaf" x={o.x + 3} y={o.y - 4} width="3" height="3" />
        <rect className="px-desk-dark" x={o.x + 1} y={o.y - 2} width="4" height="3" />
      </g>
    )
  }
  if (kind === 'mug') {
    return (
      <g data-seat-prop="mug" data-fox-part="prop" data-prop="mug" data-dir={dir}>
        <rect className="px-cream" x={o.x} y={o.y - 4} width="4" height="4" />
        <rect className="px-shade" x={o.x + 4} y={o.y - 3} width="1" height="2" />
      </g>
    )
  }
  if (kind === 'lamp') {
    return (
      <g data-seat-prop="lamp" data-fox-part="prop" data-prop="lamp" data-dir={dir}>
        <rect className="px-lamp" x={o.x} y={o.y - 6} width="6" height="3" />
        <rect className="px-ink" x={o.x + 2} y={o.y - 3} width="2" height="4" />
      </g>
    )
  }
  return (
    <g data-seat-prop="papers" data-fox-part="prop" data-prop="papers" data-dir={dir}>
      <rect className="px-white" x={o.x} y={o.y - 3} width="6" height="4" />
      <rect className="px-cream" x={o.x + 1} y={o.y - 4} width="6" height="3" />
    </g>
  )
}

function decorOrigin(dir: SeatDir): { x: number; y: number } {
  if (dir === 'down') return { x: 3, y: 26 }
  if (dir === 'up') return { x: 4, y: 12 }
  if (dir === 'left') return { x: 38, y: 18 }
  return { x: 4, y: 18 }
}

function Fox({
  dir,
  pose,
  accessory,
}: {
  dir: SeatDir
  pose: AgentStatus
  accessory: FoxAccessory
}) {
  if (dir === 'up') return <FoxUp pose={pose} accessory={accessory} />
  if (dir === 'left' || dir === 'right') return <FoxSide dir={dir} pose={pose} accessory={accessory} />
  return <FoxDown pose={pose} accessory={accessory} />
}

function FoxDown({ pose, accessory }: { pose: AgentStatus; accessory: FoxAccessory }) {
  return (
    <g data-seat-prop="fox" data-dir="down" className={pose === 'idle' ? 'px-lounge' : undefined}>
      <g data-fox-part="tail">
        <rect className="px-fur" x="34" y="16" width="6" height="3" />
        <rect className="px-cream" x="39" y="16" width="2" height="2" />
      </g>
      <rect className="px-fur" x="14" y="14" width="20" height="12" />
      <rect className="px-cream" x="20" y="18" width="8" height="3" />
      <g data-fox-part="ears">
        <rect className="px-fur" x="16" y="2" width="4" height="4" />
        <rect className="px-fur" x="28" y="2" width="4" height="4" />
        <rect className="px-shade" x="17" y="3" width="2" height="2" />
        <rect className="px-shade" x="29" y="3" width="2" height="2" />
      </g>
      <rect className="px-fur" x="16" y="5" width="16" height="8" />
      <FoxEyes pose={pose} x={20} y={7} />
      <g data-fox-part="snout">
        <rect className="px-cream" x="20" y="11" width="8" height="3" />
        <rect className="px-ink" x="23" y="12" width="2" height="1" />
      </g>
      <Accessory kind={accessory} x={18} y={4} />
      {pose === 'running' ? <Paws dir="down" /> : null}
    </g>
  )
}

function FoxUp({ pose, accessory }: { pose: AgentStatus; accessory: FoxAccessory }) {
  return (
    <g data-seat-prop="fox" data-dir="up" className={pose === 'error' ? 'px-alert' : undefined}>
      <g data-fox-part="tail">
        <rect className="px-fur" x="8" y="28" width="6" height="3" />
        <rect className="px-cream" x="6" y="29" width="3" height="2" />
      </g>
      <rect className="px-fur" x="14" y="16" width="20" height="14" />
      <rect className="px-cream" x="20" y="20" width="6" height="4" />
      <rect className="px-fur" x="18" y="34" width="4" height="4" />
      <rect className="px-fur" x="26" y="34" width="4" height="4" />
      <g data-fox-part="ears">
        <rect className="px-fur" x="16" y="10" width="4" height="3" />
        <rect className="px-fur" x="28" y="10" width="4" height="3" />
      </g>
      <rect className="px-fur" x="16" y="12" width="16" height="6" />
      <g data-fox-part="snout">
        <rect className="px-shade" x="21" y="8" width="6" height="2" />
      </g>
      <Accessory kind={accessory} x={18} y={12} />
      {pose === 'running' ? <Paws dir="up" /> : null}
    </g>
  )
}

function FoxSide({
  dir,
  pose,
  accessory,
}: {
  dir: 'left' | 'right'
  pose: AgentStatus
  accessory: FoxAccessory
}) {
  const left = dir === 'left'
  const x = (n: number, w = 0) => (left ? n : 48 - n - w)
  return (
    <g data-seat-prop="fox" data-dir={dir}>
      <g data-fox-part="tail">
        <rect className="px-fur" x={x(30, 8)} y="16" width="8" height="3" />
        <rect className="px-cream" x={x(36, 3)} y="16" width="3" height="2" />
      </g>
      <rect className="px-fur" x={x(16, 16)} y="14" width="16" height="8" />
      <rect className="px-fur" x={x(18, 3)} y="22" width="3" height="6" />
      <rect className="px-fur" x={x(26, 3)} y="22" width="3" height="5" />
      <g data-fox-part="ears">
        <rect className="px-fur" x={x(14, 4)} y="6" width="4" height="4" />
        <rect className="px-shade" x={x(15, 2)} y="7" width="2" height="2" />
      </g>
      <rect className="px-fur" x={x(10, 12)} y="10" width="12" height="8" />
      <FoxEyes pose={pose} x={x(14, 3)} y={12} wide={false} />
      <g data-fox-part="snout">
        <rect className="px-cream" x={x(4, 7)} y="13" width="7" height="4" />
        <rect className="px-ink" x={x(4, 2)} y="14" width="2" height="1" />
      </g>
      <Accessory kind={accessory} x={x(12, 6)} y={8} />
      {pose === 'running' ? <Paws dir={dir} /> : null}
    </g>
  )
}

function Paws({ dir }: { dir: SeatDir }) {
  const key = keyBox(dir)
  return (
    <g className="px-arm" data-fox-part="paws" data-dir={dir}>
      <rect className="px-fur" x={key.x} y={key.y - 2} width="4" height="3" />
      <rect className="px-shade" x={key.x} y={key.y} width="4" height="2" />
    </g>
  )
}

function FoxEyes({
  pose,
  x,
  y,
  wide = true,
}: {
  pose: AgentStatus
  x: number
  y: number
  wide?: boolean
}) {
  if (pose === 'error') {
    return (
      <g className="px-alert">
        <rect className="px-white" x={x} y={y} width="3" height="3" />
        {wide ? <rect className="px-white" x={x + 6} y={y} width="3" height="3" /> : null}
      </g>
    )
  }
  if (pose === 'running') {
    return (
      <g>
        <rect className="px-ink" x={x} y={y} width="2" height="2" />
        {wide ? <rect className="px-ink" x={x + 6} y={y} width="2" height="2" /> : null}
      </g>
    )
  }
  return (
    <g>
      <rect className="px-ink" x={x} y={y + 1} width="3" height="1" />
      {wide ? <rect className="px-ink" x={x + 6} y={y + 1} width="3" height="1" /> : null}
    </g>
  )
}

function WalkFox({ dir, accessory }: { dir: SeatDir; accessory: FoxAccessory }) {
  const left = dir === 'left'
  const x = (n: number, w = 0) => (left ? n : 34 - n - w)
  return (
    <g data-seat-prop="fox" data-dir={dir} className="px-step">
      <g data-fox-part="tail">
        <rect className="px-fur" x={x(22, 8)} y="12" width="8" height="3" />
        <rect className="px-cream" x={x(28, 3)} y="13" width="3" height="2" />
      </g>
      <rect className="px-fur" x={x(10, 14)} y="12" width="14" height="5" />
      <g data-fox-part="ears">
        <rect className="px-fur" x={x(8, 3)} y="4" width="3" height="4" />
        <rect className="px-shade" x={x(9, 2)} y="5" width="2" height="2" />
      </g>
      <rect className="px-fur" x={x(6, 10)} y="8" width="10" height="6" />
      <rect className="px-ink" x={x(8, 2)} y="10" width="2" height="2" />
      <g data-fox-part="snout">
        <rect className="px-cream" x={x(1, 6)} y="10" width="6" height="3" />
        <rect className="px-ink" x={x(1, 2)} y="11" width="2" height="1" />
      </g>
      <Accessory kind={accessory} x={x(8, 6)} y={6} />
      <g className="px-legs">
        <rect className="px-fur" x={x(12, 2)} y="17" width="2" height="4" />
        <rect className="px-fur" x={x(18, 2)} y="17" width="2" height="3" />
      </g>
    </g>
  )
}

function Accessory({ kind, x, y }: { kind: FoxAccessory; x: number; y: number }) {
  if (kind === 'none') return <g data-fox-part="accessory" data-accessory="none" />
  if (kind === 'scarf') {
    return (
      <g data-fox-part="accessory" data-accessory="scarf">
        <rect className="px-scarf" x={x} y={y + 4} width="6" height="2" />
      </g>
    )
  }
  if (kind === 'glasses') {
    return (
      <g data-fox-part="accessory" data-accessory="glasses">
        <rect className="px-ink" x={x} y={y} width="3" height="2" />
        <rect className="px-white" x={x + 1} y={y} width="1" height="1" />
        <rect className="px-ink" x={x + 4} y={y} width="3" height="2" />
        <rect className="px-white" x={x + 5} y={y} width="1" height="1" />
      </g>
    )
  }
  if (kind === 'bow') {
    return (
      <g data-fox-part="accessory" data-accessory="bow">
        <rect className="px-bow" x={x} y={y - 1} width="2" height="2" />
        <rect className="px-bow" x={x + 3} y={y - 1} width="2" height="2" />
        <rect className="px-ink" x={x + 2} y={y} width="1" height="1" />
      </g>
    )
  }
  return (
    <g data-fox-part="accessory" data-accessory="headset">
      <rect className="px-ink" x={x} y={y - 1} width="7" height="1" />
      <rect className="px-scarf" x={x} y={y} width="2" height="2" />
      <rect className="px-scarf" x={x + 5} y={y} width="2" height="2" />
    </g>
  )
}
