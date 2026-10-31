import { beforeAll, describe, expect, it } from 'vitest'
import { formatAbsolute, formatAgo, formatClock, formatHour, formatInstant, formatShort, hourOf, isTimeZone, parseTime, zoneName } from './time'

const NOW = Date.parse('2026-10-30T12:00:00Z')

// Node re-reads TZ on assignment, so "local" is New York (UTC−4 in October) for this file.
beforeAll(() => {
  process.env.TZ = 'America/New_York'
})

describe('time format', () => {
  it('reads the database format and RFC 3339 as the same instant', () => {
    expect(parseTime('2026-10-30 09:00:00')).toBe(parseTime('2026-10-30T09:00:00.000Z'))
    expect(parseTime('today')).toBeNull()
  })

  it('is relative at a glance', () => {
    expect(formatAgo('2026-10-30T11:59:30.000Z', NOW)).toBe('just now')
    expect(formatAgo('2026-10-30 11:15:00', NOW)).toBe('45m ago')
    expect(formatAgo('2026-10-30T09:00:00.000Z', NOW)).toBe('3h ago')
    expect(formatAgo('2026-10-28 12:00:00', NOW)).toBe('2d ago')
  })

  it('shows an absolute time in the chosen zone and names the zone', () => {
    expect(formatAbsolute('2026-10-30T09:00:00.000Z', 'utc')).toBe('2026-10-30 09:00:00 UTC')
    expect(formatAbsolute('2026-10-30 09:00:00', 'utc')).toBe('2026-10-30 09:00:00 UTC')
    expect(formatAbsolute('2026-10-30 09:00:00', 'local')).toBe('2026-10-30 05:00:00 EDT')
    // Midnight UTC is the evening before in New York: the date follows the zone.
    expect(formatAbsolute('2026-10-30 00:35:45', 'local')).toBe('2026-10-29 20:35:45 EDT')
    expect(formatInstant(NOW, 'utc')).toBe('2026-10-30 12:00:00 UTC')
    expect(formatInstant(NOW, 'local')).toBe('2026-10-30 08:00:00 EDT')
  })

  it('names the zone as it is at that instant', () => {
    expect(zoneName('utc')).toBe('UTC')
    expect(zoneName('local', NOW)).toBe('EDT')
    expect(zoneName('local', Date.parse('2026-01-15T12:00:00Z'))).toBe('EST')
  })

  it('formats next runs, log lines and histogram hours in the chosen zone', () => {
    const at = Date.parse('2026-10-31T16:00:00Z')
    expect(formatShort(at, 'utc')).toBe('Oct 31, 16:00 UTC')
    expect(formatShort(at, 'local')).toBe('Oct 31, 12:00 EDT')
    expect(formatClock('2026-10-31T15:23:56.719Z', 'utc')).toBe('15:23:56.719 UTC')
    expect(formatClock('2026-10-31T15:23:56.719Z', 'local')).toBe('11:23:56.719 EDT')
    expect(formatClock('2026-10-31T15:23:56Z', 'utc')).toBe('15:23:56.000 UTC')
    expect(formatHour(at, 'utc')).toBe('16:00')
    expect(formatHour(at, 'local')).toBe('12:00')
    expect(hourOf(Date.parse('2026-10-31T02:00:00Z'), 'utc')).toBe(2)
    expect(hourOf(Date.parse('2026-10-31T02:00:00Z'), 'local')).toBe(22)
    expect(formatHour(Date.parse('2026-10-31T00:00:00Z'), 'utc')).toBe('00:00')
  })

  it('returns what is not a time unchanged', () => {
    expect(formatAgo('today', NOW)).toBe('today')
    expect(formatAbsolute('today', 'utc')).toBe('today')
    expect(formatClock('today', 'local')).toBe('today')
  })

  it('accepts only the two zone settings from storage', () => {
    expect(isTimeZone('utc')).toBe(true)
    expect(isTimeZone('local')).toBe(true)
    expect(isTimeZone('Europe/Berlin')).toBe(false)
    expect(isTimeZone(null)).toBe(false)
  })
})
