import { describe, expect, it } from 'vitest'
import { foxBillboardSvg } from './foxBillboardSvg'
import { foxVariant } from '../seats'

describe('foxBillboardSvg', () => {
  it('embeds fox silhouette markers', () => {
    const svg = foxBillboardSvg(foxVariant('rustfox'), 'running')
    expect(svg).toContain('data-character="fox"')
    expect(svg).toContain('<svg')
  })
})
