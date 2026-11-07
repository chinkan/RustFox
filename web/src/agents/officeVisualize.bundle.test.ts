import { readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'

const agentsDir = dirname(fileURLToPath(import.meta.url))

function readAgentSource(name: string): string {
  return readFileSync(join(agentsDir, name), 'utf8')
}

const SVG_ENTRY_FILES = [
  '../routes/_auth/agents.tsx',
  'PixelOffice.tsx',
  'officePrefs.tsx',
  'AgentOffice.tsx',
]

const THREE_IMPORT = /from\s+['"]three['"]|@react-three\//

describe('office visualize bundle split', () => {
  it('svg entry path does not eagerly import three or r3f', () => {
    for (const rel of SVG_ENTRY_FILES) {
      const src = readFileSync(join(agentsDir, rel), 'utf8')
      expect(src, rel).not.toMatch(THREE_IMPORT)
    }
  })

  it('AgentOffice lazy-loads the office3d module', () => {
    const src = readAgentSource('AgentOffice.tsx')
    expect(src).toMatch(/import\(\s*['"]\.\/office3d\/Office3D['"]\s*\)/)
  })
})
