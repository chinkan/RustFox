import { OrthographicCamera } from '@react-three/drei'
import { useLayoutEffect, useRef } from 'react'
import type { OrthographicCamera as OrthographicCameraType } from 'three'
import type { Agent } from '../../api/types'
import { visibleDeskSlice } from '../seats'
import { DeskFox3D } from './FoxBillboard'
import { isoDeskSlots } from './isoLayout'

function FixedIsoCamera() {
  const cam = useRef<OrthographicCameraType>(null)
  useLayoutEffect(() => {
    cam.current?.lookAt(0, 0, 0)
  }, [])
  return <OrthographicCamera ref={cam} makeDefault position={[12, 12, 12]} zoom={42} near={0.1} far={200} />
}

export function Office3DScene({ agents }: { agents: Agent[] }) {
  const { visible } = visibleDeskSlice(agents)
  const slots = isoDeskSlots(visible.length)

  return (
    <>
      <FixedIsoCamera />
      <ambientLight intensity={0.65} />
      <directionalLight position={[8, 14, 6]} intensity={0.9} castShadow />

      <mesh rotation={[-Math.PI / 2, 0, 0]} receiveShadow position={[0, 0, 0]}>
        <planeGeometry args={[14, 10]} />
        <meshStandardMaterial color="#3d4f3a" />
      </mesh>

      <mesh position={[0, 1.2, -5.05]} receiveShadow>
        <boxGeometry args={[14, 2.4, 0.1]} />
        <meshStandardMaterial color="#5c4a3a" />
      </mesh>
      <mesh position={[-7.05, 1.2, 0]} receiveShadow>
        <boxGeometry args={[0.1, 2.4, 10]} />
        <meshStandardMaterial color="#6b5544" />
      </mesh>
      <mesh position={[7.05, 1.2, 0]} receiveShadow>
        <boxGeometry args={[0.1, 2.4, 10]} />
        <meshStandardMaterial color="#6b5544" />
      </mesh>

      {visible.map((agent, i) => {
        const slot = slots[i]
        if (!slot) return null
        return (
          <group key={agent.id} position={[slot.x, 0, slot.z]} rotation={[0, slot.rotationY, 0]}>
            <DeskFox3D agent={agent} />
          </group>
        )
      })}
    </>
  )
}
