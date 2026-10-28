import { lazy, Suspense, useCallback, useMemo, useState } from 'react'
import type { Agent } from '../api/types'
import { PixelOffice } from './PixelOffice'
import { OfficeViewToggle } from './OfficeViewToggle'
import { useOfficeRenderer } from './officeViewMode'
import { probeWebGL } from './webgl'

const LazyOffice3D = lazy(() =>
  import('./office3d/Office3D').then((m) => ({ default: m.Office3D })),
)

export function AgentOffice({
  agents,
  motion,
  onShowOverflow,
}: {
  agents: Agent[]
  motion: boolean
  onShowOverflow: () => void
}) {
  const [renderer, setRenderer] = useOfficeRenderer()
  const [webglBroken, setWebglBroken] = useState(false)

  const webgl = useMemo(() => probeWebGL(), [])
  const canUse3d = renderer === '3d' && motion && webgl.ok && !webglBroken

  const onFatalError = useCallback(() => setWebglBroken(true), [])

  return (
    <div className="agent-office-shell">
      <OfficeViewToggle value={renderer} onChange={setRenderer} />
      {canUse3d ? (
        <Suspense
          fallback={
            <PixelOffice agents={agents} motion={motion} onShowOverflow={onShowOverflow} />
          }
        >
          <LazyOffice3D agents={agents} onShowOverflow={onShowOverflow} onFatalError={onFatalError} />
        </Suspense>
      ) : (
        <PixelOffice agents={agents} motion={motion} onShowOverflow={onShowOverflow} />
      )}
    </div>
  )
}
