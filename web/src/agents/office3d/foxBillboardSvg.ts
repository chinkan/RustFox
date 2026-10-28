import type { AgentStatus } from '../../api/types'
import type { FoxCoat, FoxVariant } from '../seats'

const COAT_FILL: Record<FoxCoat, string> = {
  orange: '#e8a04c',
  red: '#e85d4c',
  cream: '#f3d7b0',
  silver: '#c5ced6',
  dark: '#5c4033',
}

/** Compact pixel fox (SVG) for WebGL billboards — same silhouette family as FoxSprite. */
export function foxBillboardSvg(variant: FoxVariant, pose: AgentStatus): string {
  const fur = COAT_FILL[variant.coat]
  const cream = '#f5e6c8'
  const ink = '#1a1410'
  const eyes =
    pose === 'error'
      ? `<rect x="20" y="9" width="2" height="2" fill="${ink}"/><rect x="26" y="9" width="2" height="2" fill="${ink}"/>`
      : pose === 'running'
        ? `<rect x="19" y="9" width="3" height="2" fill="${ink}"/><rect x="26" y="9" width="3" height="2" fill="${ink}"/>`
        : `<rect x="20" y="9" width="2" height="1" fill="${ink}"/><rect x="26" y="9" width="2" height="1" fill="${ink}"/>`
  const paws =
    pose === 'running'
      ? `<rect x="18" y="22" width="4" height="3" fill="${fur}"/><rect x="26" y="22" width="4" height="3" fill="${fur}"/>`
      : ''
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 48 32" shape-rendering="crispEdges">
  <rect width="48" height="32" fill="transparent"/>
  <g data-character="fox">
    <rect x="34" y="14" width="6" height="3" fill="${fur}"/>
    <rect x="39" y="14" width="2" height="2" fill="${cream}"/>
    <rect x="14" y="12" width="20" height="12" fill="${fur}"/>
    <rect x="20" y="16" width="8" height="3" fill="${cream}"/>
    <rect x="16" y="4" width="4" height="4" fill="${fur}"/>
    <rect x="28" y="4" width="4" height="4" fill="${fur}"/>
    <rect x="16" y="7" width="16" height="8" fill="${fur}"/>
    ${eyes}
    <rect x="20" y="13" width="8" height="3" fill="${cream}"/>
    <rect x="23" y="14" width="2" height="1" fill="${ink}"/>
    ${paws}
  </g>
</svg>`
}

export function foxBillboardDataUrl(variant: FoxVariant, pose: AgentStatus): string {
  const svg = foxBillboardSvg(variant, pose)
  return `data:image/svg+xml;charset=utf-8,${encodeURIComponent(svg)}`
}
