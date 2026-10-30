import { describe, expect, it } from 'vitest'
import { formatAbsolute, formatAgo, parseTime } from './time'

const NOW = Date.parse('2026-10-30T12:00:00Z')

describe('time format', () => {
  it('reads the database format and RFC 3339 as the same instant', () => {
    expect(parseTime('2026-10-30 09:00:00')).toBe(parseTime('2026-10-30T09:00:00.000Z'))
    expect(parseTime('today')).toBeNull()
  })

  it('is relative at a glance and absolute UTC on hover', () => {
    expect(formatAgo('2026-10-30T11:59:30.000Z', NOW)).toBe('just now')
    expect(formatAgo('2026-10-30 11:15:00', NOW)).toBe('45m ago')
    expect(formatAgo('2026-10-30T09:00:00.000Z', NOW)).toBe('3h ago')
    expect(formatAgo('2026-10-28 12:00:00', NOW)).toBe('2d ago')
    expect(formatAbsolute('2026-10-30T09:00:00.000Z')).toBe('2026-10-30 09:00:00 UTC')
    expect(formatAbsolute('2026-10-30 09:00:00')).toBe('2026-10-30 09:00:00 UTC')
  })

  it('returns what is not a time unchanged', () => {
    expect(formatAgo('today', NOW)).toBe('today')
    expect(formatAbsolute('today')).toBe('today')
  })
})
