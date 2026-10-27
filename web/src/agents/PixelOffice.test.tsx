import { describe, expect, it } from 'vitest'
import { render, screen, fireEvent } from '@testing-library/react'
import '../i18n'
import type { Agent } from '../api/types'
import { PixelOffice } from './PixelOffice'
import { foxVariant } from './seats'

function row(i: number, over: Partial<Agent> = {}): Agent {
  const main = i === 0
  return {
    id: main ? 'main' : `agent:seat-${i}`,
    name: main ? 'rustfox' : `seat-${i}`,
    model: 'anthropic/claude-opus-4.8',
    status: 'idle',
    lastActive: main ? '2026-10-27T00:00:00Z' : null,
    platform: main ? 'telegram+web' : 'subagent',
    live_signal: true,
    definition_only: false,
    ...over,
  }
}

describe('PixelOffice', () => {
  it('renders one desk per seat with a status pose and badge', () => {
    render(
      <PixelOffice
        agents={[
          row(0, { status: 'running' }),
          row(1, {
            name: 'verifier',
            id: 'agent:verifier',
            status: 'idle',
            live_signal: false,
            definition_only: true,
          }),
          row(2, {
            name: 'thread-writer-hk',
            id: 'agent:thread-writer-hk',
            status: 'error',
            live_signal: false,
            definition_only: true,
          }),
        ]}
        motion
        onShowOverflow={() => undefined}
      />,
    )

    const main = screen.getByRole('article', { name: /rustfox/i })
    expect(main.getAttribute('data-pose')).toBe('running')
    expect(main.textContent).toContain('Running')
    const fox = main.querySelector('svg')
    expect(fox?.getAttribute('data-character')).toBe('fox')
    expect(fox?.querySelector('[data-fox-part="ears"]')).toBeTruthy()
    expect(fox?.querySelector('[data-fox-part="snout"]')).toBeTruthy()
    expect(fox?.querySelector('[data-fox-part="tail"]')).toBeTruthy()

    const idle = screen.getByRole('article', { name: /verifier/i })
    expect(idle.getAttribute('data-pose')).toBe('idle')
    expect(idle.textContent).toContain('Idle')
    expect(idle.textContent).toContain('Definition only')
    expect(idle.textContent).toContain('No live run')
    expect(idle.querySelector('[data-fox-part="tail"]')).toBeTruthy()
    expect(idle.querySelector('[data-fox-part="ears"]')).toBeTruthy()

    const broken = screen.getByRole('article', { name: /thread-writer-hk/i })
    expect(broken.getAttribute('data-pose')).toBe('error')
    expect(broken.textContent).toContain('Error')
    expect(broken.textContent).toContain('Definition only')
    expect(broken.querySelector('[data-fox-part="tail"]')).toBeTruthy()
    expect(broken.querySelector('[data-fox-part="snout"]')).toBeTruthy()
  })

  it('does not pose a subagent as running when the payload has no live-run flag', () => {
    render(
      <PixelOffice
        agents={[
          row(1, {
            name: 'exp-executor',
            id: 'agent:exp-executor',
            status: 'running',
            live_signal: false,
            definition_only: true,
          }),
        ]}
        motion
        onShowOverflow={() => undefined}
      />,
    )
    const desk = screen.getByRole('article', { name: /exp-executor/i })
    expect(desk.getAttribute('data-pose')).toBe('idle')
    expect(desk.textContent).not.toContain('Running')
    expect(desk.textContent).toContain('Definition only')
  })

  it('poses a subagent as running only when the API marks a live run', () => {
    render(
      <PixelOffice
        agents={[
          row(1, {
            name: 'exp-executor',
            id: 'agent:exp-executor',
            status: 'running',
            live_signal: true,
            definition_only: false,
          }),
        ]}
        motion
        onShowOverflow={() => undefined}
      />,
    )
    const desk = screen.getByRole('article', { name: /exp-executor/i })
    expect(desk.getAttribute('data-pose')).toBe('running')
    expect(desk.getAttribute('data-definition-only')).toBe('false')
    expect(desk.textContent).toContain('Running')
    expect(desk.textContent).not.toContain('Definition only')
  })

  it('soft-caps visible desks and offers the overflow as +K more', () => {
    const onShowOverflow = viShow()
    render(
      <PixelOffice
        agents={Array.from({ length: 14 }, (_, i) => row(i))}
        motion
        onShowOverflow={onShowOverflow.fn}
      />,
    )
    expect(screen.getAllByRole('article')).toHaveLength(12)
    expect(screen.queryByRole('article', { name: /seat-12/i })).toBeNull()
    const more = screen.getByRole('button', { name: '+2 more' })
    fireEvent.click(more)
    expect(onShowOverflow.calls).toBe(1)
  })

  it('marks the office static when motion is off', () => {
    render(
      <PixelOffice
        agents={[row(0, { status: 'running' })]}
        motion={false}
        onShowOverflow={() => undefined}
      />,
    )
    const office = screen.getByRole('region', { name: 'Agent office' })
    expect(office.getAttribute('data-motion')).toBe('off')
    expect(screen.getByRole('article', { name: /rustfox/i }).textContent).toContain('Running')
  })

  it('marks the office animated when motion is on', () => {
    render(
      <PixelOffice agents={[row(0, { status: 'idle' })]} motion onShowOverflow={() => undefined} />,
    )
    expect(screen.getByRole('region', { name: 'Agent office' }).getAttribute('data-motion')).toBe('on')
  })

  it('roams a live idle fox through the aisle and keeps a running fox at its desk', () => {
    render(
      <PixelOffice
        agents={[
          row(0, { status: 'idle', name: 'rustfox' }),
          row(1, { status: 'running', name: 'writer', id: 'agent:writer' }),
        ]}
        motion
        onShowOverflow={() => undefined}
      />,
    )
    const office = screen.getByRole('region', { name: 'Agent office' })
    expect(office.querySelector('[data-aisle="roam"]')).toBeTruthy()

    const idle = screen.getByRole('article', { name: /rustfox/i })
    expect(idle.getAttribute('data-roaming')).toBe('true')
    expect(idle.getAttribute('data-pose')).toBe('idle')
    expect(idle.querySelector('[data-character="fox"]')).toBeNull()

    const walker = office.querySelector('[data-roam="on"] svg')
    expect(walker?.getAttribute('data-character')).toBe('fox')
    expect(walker?.querySelector('[data-fox-part="ears"]')).toBeTruthy()
    expect(walker?.querySelector('[data-fox-part="snout"]')).toBeTruthy()
    expect(walker?.querySelector('[data-fox-part="tail"]')).toBeTruthy()

    const busy = screen.getByRole('article', { name: /writer/i })
    expect(busy.getAttribute('data-roaming')).toBe('false')
    expect(busy.getAttribute('data-returning')).toBe('true')
    expect(busy.getAttribute('data-pose')).toBe('running')
    expect(busy.querySelector('[data-character="fox"]')).toBeTruthy()
  })

  it('stops roam when office motion is off and parks the fox at the desk', () => {
    render(
      <PixelOffice
        agents={[row(1, { status: 'idle', name: 'researcher', id: 'agent:researcher' })]}
        motion={false}
        onShowOverflow={() => undefined}
      />,
    )
    const office = screen.getByRole('region', { name: 'Agent office' })
    expect(office.querySelector('[data-roam="on"]')).toBeNull()
    const desk = screen.getByRole('article', { name: /researcher/i })
    expect(desk.getAttribute('data-roaming')).toBe('false')
    expect(desk.getAttribute('data-returning')).toBe('false')
    expect(desk.querySelector('[data-character="fox"]')).toBeTruthy()
    expect(desk.querySelector('[data-fox-part="tail"]')).toBeTruthy()
  })

  it('does not roam a definition-only seat', () => {
    render(
      <PixelOffice
        agents={[
          row(1, {
            name: 'verifier',
            id: 'agent:verifier',
            status: 'idle',
            live_signal: false,
            definition_only: true,
          }),
        ]}
        motion
        onShowOverflow={() => undefined}
      />,
    )
    const office = screen.getByRole('region', { name: 'Agent office' })
    expect(office.querySelector('[data-roam="on"]')).toBeNull()
    expect(screen.getByRole('article', { name: /verifier/i }).getAttribute('data-roaming')).toBe('false')
  })

  it('paints name-deterministic fox variants on the desk', () => {
    render(
      <PixelOffice
        agents={[
          row(0, { status: 'running', name: 'rustfox' }),
          row(1, { status: 'running', name: 'writer', id: 'agent:writer' }),
        ]}
        motion={false}
        onShowOverflow={() => undefined}
      />,
    )
    const rust = screen.getByRole('article', { name: /rustfox/i })
    const writer = screen.getByRole('article', { name: /writer/i })
    const rustVariant = foxVariant('rustfox')
    const writerVariant = foxVariant('writer')
    expect(rust.getAttribute('data-coat')).toBe(rustVariant.coat)
    expect(rust.getAttribute('data-accessory')).toBe(rustVariant.accessory)
    expect(rust.getAttribute('data-desk-prop')).toBe(rustVariant.deskProp)
    expect(rust.querySelector('svg')?.getAttribute('data-coat')).toBe(rustVariant.coat)
    expect(
      `${rust.getAttribute('data-coat')}|${rust.getAttribute('data-accessory')}|${rust.getAttribute('data-desk-prop')}`,
    ).not.toBe(
      `${writer.getAttribute('data-coat')}|${writer.getAttribute('data-accessory')}|${writer.getAttribute('data-desk-prop')}`,
    )
    expect(writer.getAttribute('data-coat')).toBe(writerVariant.coat)
  })

  it('seats a working fox on the chair with feet on the same line as the seat', () => {
    render(
      <PixelOffice
        agents={[row(0, { status: 'running', name: 'rustfox' })]}
        motion={false}
        onShowOverflow={() => undefined}
      />,
    )
    const svg = screen.getByRole('article', { name: /rustfox/i }).querySelector('svg')
    const chair = svg?.querySelector('[data-fox-part="chair"]')
    const paws = svg?.querySelector('[data-fox-part="paws"]')
    expect(chair?.getAttribute('data-seat-y')).toBe('26')
    expect(paws?.getAttribute('data-seat-y')).toBe('26')
    expect(svg?.querySelector('[data-fox-part="keyboard"]')).toBeTruthy()
    expect(svg?.querySelector('[data-fox-part="desk"]')).toBeTruthy()
  })

  it('frames the floor with office décor outside the roam aisle and the desks', () => {
    render(
      <PixelOffice
        agents={[
          row(0, { status: 'idle', name: 'rustfox' }),
          row(1, { status: 'running', name: 'writer', id: 'agent:writer' }),
        ]}
        motion
        onShowOverflow={() => undefined}
      />,
    )
    const office = screen.getByRole('region', { name: 'Agent office' })
    const room = office.querySelector('[data-decor="office"]')
    expect(room).toBeTruthy()
    for (const piece of ['window', 'poster', 'clock', 'door', 'rug', 'plant', 'shelf', 'cabinet', 'lamp']) {
      const tile = room?.querySelector(`[data-decor-piece="${piece}"]`)
      expect(tile?.tagName.toLowerCase()).toBe('svg')
      expect(tile?.getAttribute('shape-rendering')).toBe('crispEdges')
      const [, , vw, vh] = (tile?.getAttribute('viewBox') ?? '').split(' ').map(Number)
      expect(vw).toBeGreaterThan(0)
      expect(vh).toBeGreaterThan(0)
      const scale = 96 / 34
      expect(Number(tile?.getAttribute('width')) / vw).toBeCloseTo(scale, 5)
      expect(Number(tile?.getAttribute('height')) / vh).toBeCloseTo(scale, 5)
    }
    const aisle = office.querySelector('[data-aisle="roam"]')
    expect(aisle?.querySelector('[data-decor-piece]')).toBeNull()
    expect(office.querySelector('.office-side article')).toBeNull()
    expect(office.querySelector('[data-roam="on"]')).toBeTruthy()
    expect(screen.getByRole('article', { name: /writer/i }).getAttribute('data-returning')).toBe('true')
  })

  it('shows the empty state when no seats match', () => {
    render(<PixelOffice agents={[]} motion onShowOverflow={() => undefined} />)
    expect(screen.getByText('No agents match this filter.')).toBeTruthy()
    expect(screen.queryByRole('article')).toBeNull()
  })
})

function viShow(): { fn: () => void; calls: number } {
  const box = { fn: () => undefined, calls: 0 }
  box.fn = () => {
    box.calls += 1
  }
  return box
}
