import { Canvas } from '@react-three/fiber'
import { Suspense, useState, type ReactNode } from 'react'
import { useTranslation } from 'react-i18next'
import type { Agent } from '../../api/types'
import { SeatStatus } from '../PixelOffice'
import { seatView, visibleDeskSlice } from '../seats'
import { Office3DScene } from './Office3DScene'

export function Office3D({
  agents,
  onShowOverflow,
  onFatalError,
  fallback,
}: {
  agents: Agent[]
  onShowOverflow: () => void
  onFatalError: () => void
  /** Pixel/svg shell shown on context loss (never a blank canvas). */
  fallback: ReactNode
}) {
  const { t } = useTranslation()
  const { visible, overflow } = visibleDeskSlice(agents)
  const [glFailed, setGlFailed] = useState(false)

  const onContextLost = () => {
    setGlFailed(true)
    onFatalError()
  }

  if (glFailed) return fallback

  return (
    <div className="agent-office agent-office-3d" role="region" aria-label={t('agents.officeLabel')}>
      <p className="agent-office-hint">{t('agents.office3dHint')}</p>
      <div className="office-3d-canvas-wrap">
        <Canvas
          shadows
          dpr={[1, 1.5]}
          gl={{ antialias: false, powerPreference: 'low-power' }}
          onCreated={({ gl }) => {
            gl.domElement.addEventListener('webglcontextlost', onContextLost, { once: true })
          }}
        >
          <Suspense fallback={null}>
            <Office3DScene agents={agents} />
          </Suspense>
        </Canvas>
      </div>
      <ul className="office-3d-legend" aria-label={t('agents.officeLegend')}>
        {visible.length === 0 ? (
          <li className="empty">{t('agents.noneFound')}</li>
        ) : (
          visible.map((agent) => {
            const view = seatView(agent)
            return (
              <li
                key={agent.id}
                data-pose={view.pose}
                data-definition-only={view.definitionOnly ? 'true' : 'false'}
              >
                <strong>{agent.name}</strong>
                <SeatStatus agent={agent} />
              </li>
            )
          })
        )}
      </ul>
      {overflow > 0 ? (
        <button type="button" className="agent-desk-more office-3d-overflow" onClick={onShowOverflow}>
          {t('agents.moreDesks', { count: overflow })}
        </button>
      ) : null}
    </div>
  )
}
