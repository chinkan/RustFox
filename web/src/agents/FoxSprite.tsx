import type { AgentStatus } from '../api/types'

/**
 * RustFox at a desk. Low-res pixel fox (ears, snout, bushy white-tipped tail).
 * One pose per v1 status: idle lounges, running types, error is a distinct
 * startled fox. Not a person.
 */
export function FoxSprite({ pose }: { pose: AgentStatus }) {
  return (
    <svg
      className={`seat-sprite pose-${pose}`}
      viewBox="0 0 34 32"
      width="96"
      height="90"
      shapeRendering="crispEdges"
      aria-hidden="true"
      data-character="fox"
    >
      <Desk pose={pose} />
      {pose === 'idle' ? <IdleFox /> : pose === 'running' ? <RunningFox /> : <ErrorFox />}
    </svg>
  )
}

function Desk({ pose }: { pose: AgentStatus }) {
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
      <rect className="px-desk" x="0" y="22" width="34" height="3" />
      <rect className="px-desk-dark" x="0" y="25" width="34" height="3" />
      <rect className="px-desk-dark" x="3" y="28" width="2" height="3" />
      <rect className="px-desk-dark" x="29" y="28" width="2" height="3" />
      {pose === 'running' ? <rect className="px-key" x="13" y="20" width="10" height="2" /> : null}
    </g>
  )
}

function IdleFox() {
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
      <rect className="px-fur" x="12" y="12" width="3" height="2" />
      <rect className="px-fur" x="24" y="18" width="5" height="2" />
      <g data-fox-part="tail">
        <rect className="px-fur" x="4" y="16" width="4" height="3" />
        <rect className="px-shade" x="2" y="18" width="3" height="2" />
        <rect className="px-cream" x="1" y="19" width="2" height="2" />
      </g>
    </g>
  )
}

function RunningFox() {
  return (
    <g>
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
        <rect className="px-cream" x="15" y="12" width="2" height="1" />
      </g>
      <rect className="px-fur" x="7" y="12" width="10" height="5" />
      <rect className="px-cream" x="9" y="14" width="4" height="2" />
      <g className="px-arm">
        <rect className="px-fur" x="14" y="16" width="3" height="2" />
        <rect className="px-fur" x="18" y="16" width="3" height="2" />
        <rect className="px-shade" x="15" y="18" width="2" height="1" />
        <rect className="px-shade" x="19" y="18" width="2" height="1" />
      </g>
      <g data-fox-part="tail">
        <rect className="px-fur" x="3" y="13" width="5" height="3" />
        <rect className="px-shade" x="2" y="15" width="3" height="2" />
        <rect className="px-cream" x="1" y="16" width="2" height="2" />
      </g>
    </g>
  )
}

function ErrorFox() {
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
