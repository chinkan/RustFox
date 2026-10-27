import type { AgentStatus } from '../api/types'
import type { FoxAccessory, FoxProp, FoxVariant } from './seats'

/**
 * RustFox pixel fox (ears, snout, white-tipped tail). Not a person.
 * `walk` is the idle roam pose. Desk poses stay idle / running / error.
 */
export function FoxSprite({
  pose,
  variant,
  mode = 'seat',
}: {
  pose: AgentStatus
  variant: FoxVariant
  mode?: 'seat' | 'walk' | 'desk'
}) {
  const shown = mode === 'walk' ? 'walk' : pose
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
    >
      {mode !== 'walk' ? <Desk pose={pose} prop={variant.deskProp} /> : null}
      {mode === 'desk' ? null : shown === 'walk' ? (
        <WalkFox accessory={variant.accessory} />
      ) : pose === 'idle' ? (
        <IdleFox accessory={variant.accessory} />
      ) : pose === 'running' ? (
        <RunningFox accessory={variant.accessory} />
      ) : (
        <ErrorFox accessory={variant.accessory} />
      )}
    </svg>
  )
}

/** Seat line shared by the chair and the fox's feet. Haunches rest on y=26. */
const SEAT_Y = 26

function Desk({ pose, prop }: { pose: AgentStatus; prop: FoxProp }) {
  const screen = pose === 'running' ? 'px-screen-run' : pose === 'error' ? 'px-screen-err' : 'px-screen-idle'
  return (
    <g data-fox-part="desk">
      <rect className="px-bezel" x="17" y="0" width="14" height="8" />
      <rect className={screen} x="19" y="2" width="10" height="5" />
      {pose === 'error' ? (
        <g className="px-alert">
          <rect className="px-ink" x="23" y="3" width="2" height="2" />
          <rect className="px-ink" x="23" y="6" width="2" height="1" />
        </g>
      ) : null}
      <rect className="px-bezel" x="22" y="8" width="4" height="2" />
      <rect className="px-desk" x="4" y="12" width="40" height="6" />
      <rect className="px-desk-dark" x="4" y="18" width="40" height="3" />
      <rect className="px-desk-dark" x="6" y="21" width="3" height="6" />
      <rect className="px-desk-dark" x="39" y="21" width="3" height="6" />
      <DeskProp prop={prop} />
      {pose === 'running' ? <rect className="px-key" x="16" y="14" width="16" height="3" data-fox-part="keyboard" /> : null}
      <g data-fox-part="chair" data-seat-y={SEAT_Y}>
        <rect className="px-ink" x="16" y="24" width="16" height="2" />
        <rect className="px-key" x="14" y={SEAT_Y} width="20" height="4" />
        <rect className="px-ink" x="15" y="30" width="2" height="7" />
        <rect className="px-ink" x="31" y="30" width="2" height="7" />
      </g>
    </g>
  )
}

function DeskProp({ prop }: { prop: FoxProp }) {
  if (prop === 'plant') {
    return (
      <g data-fox-part="prop" data-prop="plant">
        <rect className="px-leaf" x="6" y="7" width="3" height="4" />
        <rect className="px-leaf" x="4" y="9" width="3" height="3" />
        <rect className="px-desk-dark" x="5" y="12" width="4" height="2" />
        <rect className="px-ink" x="5" y="14" width="4" height="1" />
      </g>
    )
  }
  if (prop === 'mug') {
    return (
      <g data-fox-part="prop" data-prop="mug">
        <rect className="px-cream" x="38" y="13" width="4" height="4" />
        <rect className="px-shade" x="42" y="14" width="1" height="2" />
      </g>
    )
  }
  if (prop === 'lamp') {
    return (
      <g data-fox-part="prop" data-prop="lamp">
        <rect className="px-lamp" x="37" y="9" width="5" height="3" />
        <rect className="px-ink" x="39" y="12" width="1" height="5" />
      </g>
    )
  }
  return (
    <g data-fox-part="prop" data-prop="papers">
      <rect className="px-white" x="36" y="13" width="6" height="4" />
      <rect className="px-cream" x="37" y="12" width="6" height="3" />
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

function IdleFox({ accessory }: { accessory: FoxAccessory }) {
  return (
    <g className="px-lounge">
      <SeatTail />
      <Haunches />
      <Feet />
      <rect className="px-fur" x="18" y="16" width="12" height="10" />
      <rect className="px-cream" x="20" y="20" width="8" height="3" />
      <Head accessory={accessory} mood="idle" />
      <rect className="px-fur" x="20" y="22" width="3" height="2" />
      <rect className="px-fur" x="25" y="22" width="3" height="2" />
    </g>
  )
}

function RunningFox({ accessory }: { accessory: FoxAccessory }) {
  return (
    <g className="px-arrive">
      <SeatTail />
      <Haunches />
      <Feet />
      <rect className="px-fur" x="18" y="15" width="12" height="11" />
      <rect className="px-cream" x="20" y="20" width="8" height="3" />
      <Head accessory={accessory} mood="running" />
      <g className="px-arm">
        <rect className="px-fur" x="17" y="15" width="4" height="2" />
        <rect className="px-fur" x="27" y="15" width="4" height="2" />
        <rect className="px-shade" x="18" y="17" width="3" height="1" />
        <rect className="px-shade" x="27" y="17" width="3" height="1" />
      </g>
    </g>
  )
}

function ErrorFox({ accessory }: { accessory: FoxAccessory }) {
  return (
    <g>
      <g className="px-alert" data-fox-part="tail">
        <rect className="px-fur" x="32" y="16" width="3" height="10" />
        <rect className="px-shade" x="32" y="22" width="2" height="2" />
        <rect className="px-cream" x="33" y="14" width="2" height="3" />
      </g>
      <Haunches />
      <Feet />
      <rect className="px-fur" x="18" y="15" width="12" height="11" />
      <Head accessory={accessory} mood="error" />
      <g className="px-alert">
        <rect className="px-fur" x="15" y="14" width="3" height="3" />
        <rect className="px-fur" x="30" y="14" width="3" height="3" />
      </g>
    </g>
  )
}

function Haunches() {
  return <rect className="px-fur" x="16" y="22" width="16" height="8" />
}

function Feet() {
  return (
    <g data-fox-part="paws" data-seat-y={SEAT_Y}>
      <rect className="px-shade" x="18" y="29" width="4" height="3" />
      <rect className="px-shade" x="26" y="29" width="4" height="3" />
    </g>
  )
}

function SeatTail() {
  return (
    <g data-fox-part="tail">
      <rect className="px-fur" x="8" y="26" width="8" height="4" />
      <rect className="px-shade" x="6" y="28" width="3" height="2" />
      <rect className="px-cream" x="4" y="29" width="3" height="2" />
    </g>
  )
}

function Head({ accessory, mood }: { accessory: FoxAccessory; mood: 'idle' | 'running' | 'error' }) {
  return (
    <g>
      <g data-fox-part="ears">
        <rect className="px-fur" x="18" y="6" width="3" height="4" />
        <rect className="px-fur" x="27" y="6" width="3" height="4" />
        <rect className="px-shade" x="19" y="8" width="1" height="2" />
        <rect className="px-shade" x="28" y="8" width="1" height="2" />
      </g>
      <rect className="px-fur" x="18" y="9" width="12" height="8" />
      {mood === 'error' ? (
        <>
          <rect className="px-white" x="20" y="11" width="3" height="3" />
          <rect className="px-white" x="25" y="11" width="3" height="3" />
          <rect className="px-ink" x="21" y="12" width="1" height="1" />
          <rect className="px-ink" x="26" y="12" width="1" height="1" />
        </>
      ) : mood === 'running' ? (
        <>
          <rect className="px-ink" x="21" y="11" width="2" height="2" />
          <rect className="px-white" x="21" y="11" width="1" height="1" />
          <rect className="px-ink" x="25" y="11" width="2" height="2" />
          <rect className="px-white" x="25" y="11" width="1" height="1" />
        </>
      ) : (
        <>
          <rect className="px-ink" x="21" y="12" width="3" height="1" />
          <rect className="px-ink" x="25" y="12" width="3" height="1" />
        </>
      )}
      <g data-fox-part="snout">
        <rect className="px-cream" x="21" y="14" width="6" height="3" />
        <rect className="px-ink" x="23" y="15" width="2" height="1" />
      </g>
      <Accessory kind={accessory} x={20} y={8} />
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
