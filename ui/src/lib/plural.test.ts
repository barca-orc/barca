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
