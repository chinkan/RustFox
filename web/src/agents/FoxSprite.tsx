import type { AgentStatus } from '../api/types'
import type { FoxAccessory, FoxProp, FoxVariant } from './seats'

/**
 * RustFox pixel fox (ears, snout, white-tipped tail). Not a person.
 * `walk` is the idle roam pose. Desk poses stay idle / running / error.
 */
export type FoxFacing = 'down' | 'up'

export function FoxSprite({
  pose,
  variant,
  mode = 'seat',
  facing = 'down',
}: {
  pose: AgentStatus
  variant: FoxVariant
  mode?: 'seat' | 'walk' | 'desk'
  /** `down` looks toward the viewer and the aisle below. `up` looks toward the aisle above. */
  facing?: FoxFacing
}) {
  const shown = mode === 'walk' ? 'walk' : pose
  const seatFacing: FoxFacing = mode === 'walk' ? 'down' : facing
  return (
    <svg
      className={`seat-sprite pose-${shown} coat-${variant.coat}`}
      viewBox={mode === 'walk' ? '0 0 34 32' : '0 0 48 40'}
      width={mode === 'walk' ? 68 : 96}
      height={mode === 'walk' ? 64 : 80}
      shapeRendering="crispEdges"
      aria-hidden="true"
      data-character={mode === 'desk' ? undefined : 'fox'}
      data-coat={variant.coat}
      data-accessory={variant.accessory}
      data-desk-prop={variant.deskProp}
      data-facing={seatFacing}
    >
      {mode !== 'walk' ? <Desk pose={pose} prop={variant.deskProp} facing={seatFacing} /> : null}
      {mode !== 'walk' && seatFacing === 'up' ? <Monitor pose={pose} facing={seatFacing} /> : null}
      {mode === 'desk' ? null : shown === 'walk' ? (
        <WalkFox accessory={variant.accessory} />
      ) : pose === 'idle' ? (
        <IdleFox accessory={variant.accessory} facing={seatFacing} />
      ) : pose === 'running' ? (
        <RunningFox accessory={variant.accessory} facing={seatFacing} />
      ) : (
        <ErrorFox accessory={variant.accessory} facing={seatFacing} />
      )}
      {mode !== 'walk' && seatFacing === 'down' ? <Monitor pose={pose} facing={seatFacing} /> : null}
    </svg>
  )
}

/**
 * Up row: chair at y=26, monitor standing on the desk toward the aisle.
 * Down row: chair behind the fox at y=10, monitor standing on the desk toward the aisle.
 * The stand's bottom edge lands inside the desk top. The screen never drops into the leg bay.
 */
const SEAT_Y_UP = 26
const SEAT_Y_DOWN = 10

function Desk({ pose, prop, facing }: { pose: AgentStatus; prop: FoxProp; facing: FoxFacing }) {
  const down = facing === 'down'
  const seat = down ? SEAT_Y_DOWN : SEAT_Y_UP
  return (
    <g data-fox-part="desk" data-facing={facing}>
      <rect className="px-desk" data-fox-part="desk-top" x="4" y={down ? 20 : 12} width="40" height="6" />
      <rect className="px-desk-dark" x="4" y={down ? 26 : 18} width="40" height="2" />
      <rect className="px-desk-dark" x="6" y={down ? 26 : 20} width="3" height="6" />
      <rect className="px-desk-dark" x="39" y={down ? 26 : 20} width="3" height="6" />
      <DeskProp prop={prop} facing={facing} />
      {pose === 'running' ? (
        <rect className="px-key" x="16" y={down ? 21 : 14} width="16" height="3" data-fox-part="keyboard" />
      ) : null}
      <g data-fox-part="chair" data-seat-y={seat}>
        <rect className="px-ink" x="16" y={down ? 0 : 24} width="16" height="2" />
        <rect className="px-key" x="14" y={seat} width="20" height="4" />
        <rect className="px-ink" x="15" y={down ? 14 : 30} width="2" height="6" />
        <rect className="px-ink" x="31" y={down ? 14 : 30} width="2" height="6" />
      </g>
    </g>
  )
}

function Monitor({ pose, facing }: { pose: AgentStatus; facing: FoxFacing }) {
  const screen = pose === 'running' ? 'px-screen-run' : pose === 'error' ? 'px-screen-err' : 'px-screen-idle'
  const down = facing === 'down'
  // Down: screen sits in front of the torso, stand planted on desk y=20.
  // Up: screen stays above the desk, stand reaches desk y=12.
  const bezelY = down ? 12 : 0
  const standY = down ? 20 : 10
  const standH = down ? 2 : 3
  return (
    <g data-fox-part="monitor-rig" data-facing={facing}>
      <rect className="px-bezel" data-fox-part="monitor" x="17" y={bezelY} width="14" height="8" />
      <rect className={screen} x="19" y={bezelY + 2} width="10" height="5" />
      {pose === 'error' ? (
        <g className="px-alert">
          <rect className="px-ink" x="23" y={bezelY + 3} width="2" height="2" />
          <rect className="px-ink" x="23" y={bezelY + 6} width="2" height="1" />
        </g>
      ) : null}
      <rect
        className="px-bezel"
        data-fox-part="monitor-stand"
        x="22"
        y={standY}
        width="4"
        height={standH}
      />
    </g>
  )
}

function DeskProp({ prop, facing }: { prop: FoxProp; facing: FoxFacing }) {
  const dy = facing === 'down' ? 8 : 0
  if (prop === 'plant') {
    return (
      <g data-fox-part="prop" data-prop="plant">
        <rect className="px-leaf" x="6" y={7 + dy} width="3" height="4" />
        <rect className="px-leaf" x="4" y={9 + dy} width="3" height="3" />
        <rect className="px-desk-dark" x="5" y={12 + dy} width="4" height="2" />
        <rect className="px-ink" x="5" y={14 + dy} width="4" height="1" />
      </g>
    )
  }
  if (prop === 'mug') {
    return (
      <g data-fox-part="prop" data-prop="mug">
        <rect className="px-cream" x="38" y={13 + dy} width="4" height="4" />
        <rect className="px-shade" x="42" y={14 + dy} width="1" height="2" />
      </g>
    )
  }
  if (prop === 'lamp') {
    return (
      <g data-fox-part="prop" data-prop="lamp">
        <rect className="px-lamp" x="37" y={9 + dy} width="5" height="3" />
        <rect className="px-ink" x="39" y={12 + dy} width="1" height="5" />
      </g>
    )
  }
  return (
    <g data-fox-part="prop" data-prop="papers">
      <rect className="px-white" x="36" y={13 + dy} width="6" height="4" />
      <rect className="px-cream" x="37" y={12 + dy} width="6" height="3" />
    </g>
  )
}

function Accessory({
  kind,
  x,
  y,
}: {
  kind: FoxAccessory
  x: number
  y: number
}) {
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

function IdleFox({ accessory, facing }: { accessory: FoxAccessory; facing: FoxFacing }) {
  const down = facing === 'down'
  return (
    <g className="px-lounge">
      <SeatTail facing={facing} />
      <Haunches facing={facing} />
      <Feet facing={facing} />
      <rect className="px-fur" x="18" y={down ? 12 : 16} width="12" height={down ? 8 : 10} />
      {down ? <rect className="px-cream" x="20" y="15" width="8" height="3" /> : null}
      <Head accessory={accessory} mood="idle" facing={facing} />
      <rect className="px-fur" x="20" y={down ? 16 : 22} width="3" height="2" />
      <rect className="px-fur" x="25" y={down ? 16 : 22} width="3" height="2" />
    </g>
  )
}

function RunningFox({ accessory, facing }: { accessory: FoxAccessory; facing: FoxFacing }) {
  const down = facing === 'down'
  return (
    <g className="px-arrive">
      <SeatTail facing={facing} />
      <Haunches facing={facing} />
      <Feet facing={facing} />
      <rect className="px-fur" x="18" y={down ? 12 : 15} width="12" height={down ? 8 : 11} />
      {down ? <rect className="px-cream" x="20" y="15" width="8" height="3" /> : null}
      <Head accessory={accessory} mood="running" facing={facing} />
      <g className="px-arm">
        <rect className="px-fur" x="17" y={down ? 19 : 12} width="4" height="3" />
        <rect className="px-fur" x="27" y={down ? 19 : 12} width="4" height="3" />
        <rect className="px-shade" x="18" y={down ? 21 : 14} width="3" height="1" />
        <rect className="px-shade" x="27" y={down ? 21 : 14} width="3" height="1" />
      </g>
    </g>
  )
}

function ErrorFox({ accessory, facing }: { accessory: FoxAccessory; facing: FoxFacing }) {
  const down = facing === 'down'
  return (
    <g>
      <g className="px-alert" data-fox-part="tail">
        {down ? (
          <>
            <rect className="px-fur" x="32" y="2" width="3" height="8" />
            <rect className="px-shade" x="32" y="6" width="2" height="2" />
            <rect className="px-cream" x="33" y="0" width="2" height="3" />
          </>
        ) : (
          <>
            <rect className="px-fur" x="32" y="16" width="3" height="10" />
            <rect className="px-shade" x="32" y="22" width="2" height="2" />
            <rect className="px-cream" x="33" y="14" width="2" height="3" />
          </>
        )}
      </g>
      <Haunches facing={facing} />
      <Feet facing={facing} />
      <rect className="px-fur" x="18" y={down ? 12 : 15} width="12" height={down ? 8 : 11} />
      <Head accessory={accessory} mood="error" facing={facing} />
      <g className="px-alert">
        <rect className="px-fur" x="15" y={down ? 4 : 12} width="3" height="3" />
        <rect className="px-fur" x="30" y={down ? 4 : 12} width="3" height="3" />
      </g>
    </g>
  )
}

function Haunches({ facing }: { facing: FoxFacing }) {
  const down = facing === 'down'
  return <rect className="px-fur" x="16" y={down ? 6 : 22} width="16" height="8" />
}

function Feet({ facing }: { facing: FoxFacing }) {
  const down = facing === 'down'
  return (
    <g data-fox-part="paws" data-seat-y={down ? SEAT_Y_DOWN : SEAT_Y_UP}>
      <rect className="px-shade" x={down ? 14 : 18} y={down ? 12 : 29} width="4" height="3" />
      <rect className="px-shade" x={down ? 30 : 26} y={down ? 12 : 29} width="4" height="3" />
    </g>
  )
}

function SeatTail({ facing }: { facing: FoxFacing }) {
  const down = facing === 'down'
  return (
    <g data-fox-part="tail">
      <rect className="px-fur" x="8" y={down ? 10 : 26} width="8" height="4" />
      <rect className="px-shade" x="6" y={down ? 12 : 28} width="3" height="2" />
      <rect className="px-cream" x="4" y={down ? 13 : 29} width="3" height="2" />
    </g>
  )
}

function Head({
  accessory,
  mood,
  facing,
}: {
  accessory: FoxAccessory
  mood: 'idle' | 'running' | 'error'
  facing: FoxFacing
}) {
  if (facing === 'up') {
    return (
      <g data-facing="up">
        <rect className="px-fur" x="18" y="12" width="12" height="6" />
        <g data-fox-part="ears">
          <rect className="px-fur" x="17" y="10" width="4" height="3" />
          <rect className="px-fur" x="27" y="10" width="4" height="3" />
          <rect className="px-shade" x="18" y="11" width="2" height="2" />
          <rect className="px-shade" x="28" y="11" width="2" height="2" />
        </g>
        <rect className="px-ink" x="21" y="11" width="2" height="2" />
        <rect className="px-white" x="21" y="11" width="1" height="1" />
        <rect className="px-ink" x="25" y="11" width="2" height="2" />
        <rect className="px-white" x="25" y="11" width="1" height="1" />
        <g data-fox-part="snout">
          <rect className="px-cream" x="21" y="6" width="6" height="5" />
          <rect className="px-ink" x="23" y="6" width="2" height="2" />
        </g>
        <Accessory kind={accessory} x={19} y={12} />
      </g>
    )
  }
  return (
    <g data-facing="down">
      <g data-fox-part="ears">
        <rect className="px-fur" x="18" y="1" width="3" height="3" />
        <rect className="px-fur" x="27" y="1" width="3" height="3" />
        <rect className="px-shade" x="19" y="2" width="1" height="2" />
        <rect className="px-shade" x="28" y="2" width="1" height="2" />
      </g>
      <rect className="px-fur" x="18" y="3" width="12" height="7" />
      {mood === 'error' ? (
        <>
          <rect className="px-white" x="20" y="5" width="3" height="3" />
          <rect className="px-white" x="25" y="5" width="3" height="3" />
          <rect className="px-ink" x="21" y="6" width="1" height="1" />
          <rect className="px-ink" x="26" y="6" width="1" height="1" />
        </>
      ) : mood === 'running' ? (
        <>
          <rect className="px-ink" x="21" y="5" width="2" height="2" />
          <rect className="px-white" x="21" y="5" width="1" height="1" />
          <rect className="px-ink" x="25" y="5" width="2" height="2" />
          <rect className="px-white" x="25" y="5" width="1" height="1" />
        </>
      ) : (
        <>
          <rect className="px-ink" x="21" y="5" width="3" height="1" />
          <rect className="px-ink" x="25" y="5" width="3" height="1" />
        </>
      )}
      <g data-fox-part="snout">
        <rect className="px-cream" x="21" y="8" width="6" height="3" />
        <rect className="px-ink" x="23" y="9" width="2" height="1" />
      </g>
      <Accessory kind={accessory} x={20} y={4} />
    </g>
  )
}

function WalkFox({ accessory }: { accessory: FoxAccessory }) {
  return (
    <g className="px-step">
      <g data-fox-part="tail">
        <rect className="px-fur" x="1" y="12" width="6" height="3" />
        <rect className="px-cream" x="0" y="13" width="2" height="2" />
      </g>
      <rect className="px-fur" x="7" y="12" width="10" height="4" />
      <rect className="px-fur" x="8" y="7" width="8" height="5" />
      <g data-fox-part="ears">
        <rect className="px-fur" x="10" y="3" width="2" height="3" />
        <rect className="px-fur" x="9" y="5" width="4" height="2" />
        <rect className="px-shade" x="10" y="5" width="2" height="1" />
      </g>
      <rect className="px-ink" x="13" y="8" width="2" height="2" />
      <g data-fox-part="snout">
        <rect className="px-cream" x="15" y="8" width="4" height="3" />
        <rect className="px-ink" x="18" y="9" width="1" height="1" />
      </g>
      <Accessory kind={accessory} x={11} y={6} />
      <g className="px-legs">
        <rect className="px-fur" x="8" y="16" width="2" height="4" />
        <rect className="px-fur" x="14" y="16" width="2" height="3" />
      </g>
    </g>
  )
}
