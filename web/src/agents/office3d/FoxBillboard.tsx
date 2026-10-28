import { Billboard } from '@react-three/drei'
import { useLoader } from '@react-three/fiber'
import { useMemo } from 'react'
import * as THREE from 'three'
import type { Agent } from '../../api/types'
import { foxBillboardDataUrl } from './foxBillboardSvg'
import { foxVariant, seatView } from '../seats'

function FoxTexture({ agent }: { agent: Agent }) {
  const view = seatView(agent)
  const variant = foxVariant(agent.name)
  const url = useMemo(() => foxBillboardDataUrl(variant, view.pose), [agent.name, view.pose])
  const texture = useLoader(THREE.TextureLoader, url)
  texture.magFilter = THREE.NearestFilter
  texture.minFilter = THREE.NearestFilter
  return (
    <Billboard follow lockX lockY lockZ position={[0, 0.55, 0.35]}>
      <mesh>
        <planeGeometry args={[0.9, 0.6]} />
        <meshBasicMaterial map={texture} transparent toneMapped={false} />
      </mesh>
    </Billboard>
  )
}

export function DeskFox3D({ agent }: { agent: Agent }) {
  const view = seatView(agent)
  return (
    <group data-pose={view.pose} data-definition-only={view.definitionOnly ? 'true' : 'false'}>
      <mesh position={[0, 0.42, 0]} castShadow>
        <boxGeometry args={[1.1, 0.08, 0.7]} />
        <meshStandardMaterial color="#4a3f35" />
      </mesh>
      <mesh position={[0, 0.62, -0.15]} castShadow>
        <boxGeometry args={[0.5, 0.35, 0.06]} />
        <meshStandardMaterial
          color="#2a3340"
          emissive={view.pose === 'running' ? '#1a4a3a' : '#000000'}
          emissiveIntensity={view.pose === 'running' ? 0.35 : 0}
        />
      </mesh>
      <FoxTexture agent={agent} />
    </group>
  )
}
