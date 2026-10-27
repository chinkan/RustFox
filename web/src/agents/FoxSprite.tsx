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
      viewBox="0 0 34 32"
      width={mode === 'walk' ? 64 : 88}
      height={mode === 'walk' ? 52 : 82}
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

function Desk({ pose, prop }: { pose: AgentStatus; prop: FoxProp }) {
  const screen = pose === 'running' ? 'px-screen-run' : pose === 'error' ? 'px-screen-err' : 'px-screen-idle'
  return (
    <g data-fox-part="desk">
      <rect className="px-bezel" x="22" y="2" width="10" height="9" />
      <rect className={screen} x="24" y="4" width="6" height="5" />
      {pose === 'error' ? (
        <g className="px-alert">
          <rect className="px-ink" x="26" y="5" width="1" height="2" />
          <rect className="px-ink" x="26" y="8" width="1" height="1" />
        </g>
      ) : null}
      <rect className="px-bezel" x="26" y="11" width="2" height="3" />
      <DeskProp prop={prop} />
      <rect className="px-desk" x="0" y="22" width="34" height="3" />
      <rect className="px-desk-dark" x="0" y="25" width="34" height="3" />
      <rect className="px-desk-dark" x="3" y="28" width="2" height="3" />
      <rect className="px-desk-dark" x="29" y="28" width="2" height="3" />
      {pose === 'running' ? <rect className="px-key" x="13" y="20" width="10" height="2" /> : null}
    </g>
  )
}

function DeskProp({ prop }: { prop: FoxProp }) {
  if (prop === 'plant') {
    return (
      <g data-fox-part="prop" data-prop="plant">
        <rect className="px-leaf" x="20" y="14" width="2" height="3" />
        <rect className="px-leaf" x="18" y="15" width="2" height="2" />
        <rect className="px-desk-dark" x="19" y="17" width="3" height="2" />
      </g>
    )
  }
  if (prop === 'mug') {
    return (
      <g data-fox-part="prop" data-prop="mug">
        <rect className="px-cream" x="19" y="15" width="3" height="3" />
        <rect className="px-shade" x="22" y="16" width="1" height="2" />
      </g>
    )
  }
  if (prop === 'lamp') {
    return (
      <g data-fox-part="prop" data-prop="lamp">
        <rect className="px-lamp" x="19" y="13" width="3" height="2" />
        <rect className="px-ink" x="20" y="15" width="1" height="4" />
      </g>
    )
  }
  return (
    <g data-fox-part="prop" data-prop="papers">
      <rect className="px-white" x="18" y="16" width="4" height="3" />
      <rect className="px-cream" x="19" y="15" width="4" height="2" />
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
      <rect className="px-desk-dark" x="1" y="8" width="3" height="14" />
      <rect className="px-fur" x="6" y="14" width="16" height="4" />
      <rect className="px-cream" x="10" y="16" width="6" height="2" />
      <rect className="px-fur" x="16" y="9" width="8" height="5" />
      <g data-fox-part="ears">
        <rect className="px-fur" x="16" y="6" width="2" height="2" />
        <rect className="px-fur" x="15" y="8" width="4" height="2" />
        <rect className="px-shade" x="16" y="8" width="2" height="1" />
      </g>
      <rect className="px-ink" x="20" y="11" width="3" height="1" />
      <g data-fox-part="snout">
        <rect className="px-cream" x="22" y="10" width="5" height="3" />
        <rect className="px-ink" x="26" y="11" width="1" height="1" />
      </g>
      <Accessory kind={accessory} x={18} y={8} />
      <rect className="px-fur" x="12" y="12" width="3" height="2" />
      <rect className="px-fur" x="24" y="18" width="5" height="2" />
      <g data-fox-part="tail">
        <rect className="px-fur" x="3" y="15" width="5" height="4" />
        <rect className="px-shade" x="1" y="18" width="3" height="2" />
        <rect className="px-cream" x="0" y="19" width="2" height="2" />
      </g>
    </g>
  )
}

function RunningFox({ accessory }: { accessory: FoxAccessory }) {
  return (
    <g className="px-arrive">
      <g data-fox-part="ears">
        <rect className="px-fur" x="11" y="3" width="2" height="2" />
        <rect className="px-fur" x="10" y="5" width="4" height="2" />
        <rect className="px-shade" x="11" y="5" width="2" height="1" />
      </g>
      <rect className="px-fur" x="8" y="7" width="9" height="5" />
      <rect className="px-ink" x="13" y="8" width="2" height="2" />
      <rect className="px-white" x="13" y="8" width="1" height="1" />
      <g data-fox-part="snout">
        <rect className="px-cream" x="15" y="9" width="4" height="3" />
        <rect className="px-ink" x="18" y="10" width="1" height="1" />
      </g>
      <Accessory kind={accessory} x={11} y={7} />
      <rect className="px-fur" x="7" y="12" width="10" height="5" />
      <rect className="px-cream" x="9" y="14" width="4" height="2" />
      <g className="px-arm">
        <rect className="px-fur" x="14" y="16" width="3" height="2" />
        <rect className="px-fur" x="18" y="16" width="3" height="2" />
        <rect className="px-shade" x="15" y="18" width="2" height="1" />
        <rect className="px-shade" x="19" y="18" width="2" height="1" />
      </g>
      <g data-fox-part="tail">
        <rect className="px-fur" x="2" y="12" width="6" height="3" />
        <rect className="px-shade" x="1" y="14" width="3" height="2" />
        <rect className="px-cream" x="0" y="15" width="2" height="2" />
      </g>
    </g>
  )
}

function ErrorFox({ accessory }: { accessory: FoxAccessory }) {
  return (
    <g>
      <g data-fox-part="ears">
        <rect className="px-fur" x="4" y="8" width="5" height="2" />
        <rect className="px-shade" x="4" y="9" width="3" height="1" />
      </g>
      <rect className="px-fur" x="9" y="6" width="9" height="6" />
      <rect className="px-white" x="11" y="8" width="2" height="2" />
      <rect className="px-white" x="15" y="8" width="2" height="2" />
      <rect className="px-ink" x="12" y="9" width="1" height="1" />
      <rect className="px-ink" x="16" y="9" width="1" height="1" />
      <g data-fox-part="snout">
        <rect className="px-cream" x="15" y="11" width="4" height="2" />
        <rect className="px-ink" x="18" y="12" width="1" height="1" />
      </g>
      <Accessory kind={accessory} x={10} y={7} />
      <rect className="px-fur" x="10" y="12" width="8" height="5" />
      <g className="px-alert">
        <rect className="px-fur" x="7" y="10" width="2" height="3" />
        <rect className="px-fur" x="18" y="10" width="2" height="3" />
      </g>
      <g data-fox-part="tail">
        <rect className="px-fur" x="18" y="1" width="3" height="6" />
        <rect className="px-shade" x="18" y="3" width="2" height="2" />
        <rect className="px-cream" x="19" y="0" width="2" height="2" />
      </g>
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
