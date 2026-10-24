import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { describe, expect, it } from 'vitest'
import vm from 'node:vm'

/**
 * ADR-0043 Phase 1 wizard chrome is plain HTML. Lock the three status
 * strings and that OpenCode is never a provider radio or thin-save value.
 */
const html = readFileSync(resolve(process.cwd(), '../setup/index.html'), 'utf8')

function loadStatusText(): (data: {
  state?: string
  model_count?: number
  message?: string | null
}) => string {
  const start = html.indexOf('function opencodeStatusText')
  const end = html.indexOf('async function loadOpenCodeStatus')
  const src = html.slice(start, end)
  return vm.runInNewContext(`${src}\nopencodeStatusText`) as (data: {
    state?: string
    model_count?: number
    message?: string | null
  }) => string
}

describe('setup wizard OpenCode status strip', () => {
  const opencodeStatusText = loadStatusText()

  it('renders the three detect-only lines', () => {
    expect(opencodeStatusText({ state: 'absent', message: 'opencode binary not found', model_count: 3 }))
      .toBe('OpenCode: not installed — opencode binary not found')
    expect(opencodeStatusText({ state: 'no_creds', message: null, model_count: 4 }))
      .toBe('OpenCode: installed, no credentials')
    expect(opencodeStatusText({ state: 'ready', model_count: 2, message: null }))
      .toBe('OpenCode: ready — 2 models')
  })

  it('is a status strip under the two provider radios, never a third choice', () => {
    expect(html).toContain('id="opencode-status"')
    expect(html).toContain('fetch(\'/api/opencode/status\')')
    expect(html.match(/name="provider"/g)).toHaveLength(2)
    expect(html).toContain('value="openrouter"')
    expect(html).toContain('value="ollama"')
    expect(html).not.toContain('value="opencode"')
    expect(html).not.toMatch(/provider:\s*'opencode'/)
    expect(html).not.toMatch(/state\.provider\s*=\s*'opencode'/)
    const radios = html.indexOf('name="provider"')
    const strip = html.indexOf('id="opencode-status"')
    const openrouterFields = html.indexOf('id="openrouter-fields"')
    expect(radios).toBeGreaterThan(-1)
    expect(strip).toBeGreaterThan(radios)
    expect(openrouterFields).toBeGreaterThan(strip)
  })
})
