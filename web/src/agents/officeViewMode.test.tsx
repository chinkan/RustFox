import { beforeEach, describe, expect, it } from 'vitest'
import {
  __resetOfficeRendererForTests,
  getOfficeRenderer,
  OFFICE_RENDERER_SESSION_KEY,
  setOfficeRenderer,
} from './officeViewMode'

beforeEach(() => {
  sessionStorage.clear()
  __resetOfficeRendererForTests()
})

describe('office renderer session toggle', () => {
  it('defaults to pixel', () => {
    expect(getOfficeRenderer()).toBe('pixel')
  })

  it('stores 3d in sessionStorage only', () => {
    setOfficeRenderer('3d')
    expect(sessionStorage.getItem(OFFICE_RENDERER_SESSION_KEY)).toBe('3d')
    expect(getOfficeRenderer()).toBe('3d')
    setOfficeRenderer('pixel')
    expect(sessionStorage.getItem(OFFICE_RENDERER_SESSION_KEY)).toBeNull()
    expect(getOfficeRenderer()).toBe('pixel')
  })
})
