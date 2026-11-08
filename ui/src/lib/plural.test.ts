import { describe, expect, it } from 'vitest'
import { plural } from './plural'

describe('plural', () => {
  it('picks the noun by count', () => {
    expect(plural(1, 'run')).toBe('1 run')
    expect(plural(0, 'run')).toBe('0 runs')
    expect(plural(1200, 'key')).toBe('1,200 keys')
    expect(plural(2, 'query', 'queries')).toBe('2 queries')
  })
})

describe('compactCount', () => {
  it('shortens thousands and millions to fit a node', async () => {
    const { compactCount } = await import('./plural')
    expect([999, 1250, 10_000, 250_000, 1_000_000, 2_450_000].map(compactCount)).toEqual(['999', '1.3k', '10k', '250k', '1M', '2.5M'])
  })
})
