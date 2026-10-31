import { render, screen } from '@testing-library/react'
import { describe, expect, it, vi } from 'vitest'
import { Office3DErrorBoundary } from './Office3DErrorBoundary'

function Boom(): null {
  throw new Error('r3f boom')
}

describe('Office3DErrorBoundary', () => {
  it('renders fallback and notifies parent on child throw', () => {
    const onError = vi.fn()
    render(
      <Office3DErrorBoundary onError={onError} fallback={<div data-testid="pixel-fallback">pixel</div>}>
        <Boom />
      </Office3DErrorBoundary>,
    )
    expect(screen.getByTestId('pixel-fallback')).toBeTruthy()
    expect(onError).toHaveBeenCalled()
  })
})
