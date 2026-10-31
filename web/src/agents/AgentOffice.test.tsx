import { fireEvent, render, screen } from '@testing-library/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import '../i18n'
import type { Agent } from '../api/types'
import { AgentOffice } from './AgentOffice'
import { __resetOfficePrefsForTests, writeOfficePrefs } from './officePrefs'
import { probeWebGL } from './webgl'

vi.mock('./webgl', () => ({
  probeWebGL: vi.fn(() => ({ ok: true })),
}))

vi.mock('./office3d/Office3D', () => ({
  Office3D: () => <div data-testid="office-3d-mock">3D office</div>,
}))

const agent: Agent = {
  id: 'main',
  name: 'rustfox',
  model: 'test',
  status: 'idle',
  lastActive: null,
  platform: 'telegram+web',
  live_signal: true,
  definition_only: false,
}

beforeEach(() => {
  localStorage.clear()
  __resetOfficePrefsForTests()
  vi.mocked(probeWebGL).mockReturnValue({ ok: true })
})

describe('AgentOffice', () => {
  it('shows Pixel office by default with Office view toggle copy', () => {
    render(<AgentOffice agents={[agent]} motion onShowOverflow={() => undefined} />)
    expect(screen.getByRole('group', { name: /Office view/i })).toBeTruthy()
    expect(screen.getByRole('region', { name: /Agent office/i })).toBeTruthy()
    expect(screen.queryByTestId('office-3d-mock')).toBeNull()
  })

  it('lazy-loads 3D when settings office_visualize is webgl', async () => {
    writeOfficePrefs({ reduceMotion: false, disableOfficeAnimations: false, office_visualize: 'webgl' })
    render(<AgentOffice agents={[agent]} motion onShowOverflow={() => undefined} />)
    expect(await screen.findByTestId('office-3d-mock')).toBeTruthy()
  })

  it('falls back to Pixel when WebGL probe fails (effective visualize stays svg)', () => {
    vi.mocked(probeWebGL).mockReturnValue({ ok: false, reason: 'no-context' })
    writeOfficePrefs({ reduceMotion: false, disableOfficeAnimations: false, office_visualize: 'webgl' })
    render(<AgentOffice agents={[agent]} motion onShowOverflow={() => undefined} />)
    expect(screen.getByRole('region', { name: /Agent office/i })).toBeTruthy()
    expect(screen.queryByTestId('office-3d-mock')).toBeNull()
    expect(screen.getByText(/Idle foxes roam/i)).toBeTruthy()
  })

  it('falls back to Pixel when motion is disabled (reduced motion path)', () => {
    writeOfficePrefs({ reduceMotion: false, disableOfficeAnimations: false, office_visualize: 'webgl' })
    render(<AgentOffice agents={[agent]} motion={false} onShowOverflow={() => undefined} />)
    expect(screen.getByRole('region', { name: /Agent office/i })).toBeTruthy()
    expect(screen.queryByTestId('office-3d-mock')).toBeNull()
  })

  it('switches renderer via toggle buttons and persists webgl', async () => {
    render(<AgentOffice agents={[agent]} motion onShowOverflow={() => undefined} />)
    fireEvent.click(screen.getByRole('button', { name: '3D' }))
    expect(await screen.findByTestId('office-3d-mock')).toBeTruthy()
    expect(localStorage.getItem('rustfox.portal.office')).toContain('"office_visualize":"webgl"')
    fireEvent.click(screen.getByRole('button', { name: 'Pixel' }))
    expect(screen.queryByTestId('office-3d-mock')).toBeNull()
    expect(localStorage.getItem('rustfox.portal.office')).toContain('"office_visualize":"svg"')
  })
})
