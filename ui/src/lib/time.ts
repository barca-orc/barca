/**
 * The one time format: relative where it is read at a glance, absolute in the reader's chosen
 * zone everywhere else. The server writes UTC `YYYY-MM-DD HH:MM:SS` (SQLite `datetime('now')`)
 * or RFC 3339; both read the same. Every absolute time names its zone (`UTC`, or the reader's
 * own as the browser abbreviates it: `PDT`, `GMT+1`, `GMT+5:30`), so a screen never mixes zones
 * silently. The zone is a setting (`useTimeZone`), persisted next to the theme; `local` is the
 * default.
 */

export type TimeZone = 'local' | 'utc'

export const TIME_ZONES: readonly TimeZone[] = ['local', 'utc']

export function isTimeZone(value: unknown): value is TimeZone {
  return value === 'local' || value === 'utc'
}

const DATABASE_TIME = /^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}$/

/** Ms since epoch; null when `value` is not a time. */
export function parseTime(value: string): number | null {
  const t = Date.parse(DATABASE_TIME.test(value) ? `${value.replace(' ', 'T')}Z` : value)
  return Number.isNaN(t) ? null : t
}

/** `3h ago`; a value that is not a time is returned as it is. */
export function formatAgo(value: string, nowMs: number): string {
  const t = parseTime(value)
  if (t === null) return value
  const s = Math.max(0, (nowMs - t) / 1000)
  if (s < 60) return 'just now'
  if (s < 3600) return `${Math.floor(s / 60)}m ago`
  if (s < 86400) return `${Math.floor(s / 3600)}h ago`
  return `${Math.floor(s / 86400)}d ago`
}

/** The IANA zone `Intl` formats in; `undefined` is the browser's own. */
const zoneId = (zone: TimeZone) => (zone === 'utc' ? 'UTC' : undefined)

const formatters = new Map<string, Intl.DateTimeFormat>()

/** One `Intl.DateTimeFormat` per zone and option set; building one is the slow part. */
function formatter(zone: TimeZone, options: Intl.DateTimeFormatOptions): Intl.DateTimeFormat {
  const key = `${zone}|${JSON.stringify(options)}`
  let f = formatters.get(key)
  if (!f) {
    f = new Intl.DateTimeFormat('en-US', { timeZone: zoneId(zone), hourCycle: 'h23', ...options })
    formatters.set(key, f)
  }
  return f
}

type Parts = Partial<Record<Intl.DateTimeFormatPartTypes, string>>

function parts(ms: number, zone: TimeZone, options: Intl.DateTimeFormatOptions): Parts {
  const out: Parts = {}
  for (const p of formatter(zone, options).formatToParts(new Date(ms))) out[p.type] = p.value
  return out
}

/** The zone's abbreviation at `ms`: `UTC`, or the browser's name for its own (`PDT`, `GMT+1`). */
export function zoneName(zone: TimeZone, ms = Date.now()): string {
  if (zone === 'utc') return 'UTC'
  return parts(ms, zone, { timeZoneName: 'short' }).timeZoneName ?? 'local'
}

const DATE_TIME: Intl.DateTimeFormatOptions = {
  year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', second: '2-digit',
}

/** `2026-10-30 09:00:00 UTC` / `2026-10-30 02:00:00 PDT`. */
export function formatInstant(ms: number, zone: TimeZone): string {
  const p = parts(ms, zone, DATE_TIME)
  return `${p.year}-${p.month}-${p.day} ${p.hour}:${p.minute}:${p.second} ${zoneName(zone, ms)}`
}

/** `formatInstant` of a time as the server writes it; a value that is not a time is returned as it is. */
export function formatAbsolute(value: string, zone: TimeZone): string {
  const t = parseTime(value)
  return t === null ? value : formatInstant(t, zone)
}

/** `Oct 31, 12:00 PDT`: a time near enough that the year goes without saying (next run). */
export function formatShort(ms: number, zone: TimeZone): string {
  const p = parts(ms, zone, { month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' })
  return `${p.month} ${p.day}, ${p.hour}:${p.minute} ${zoneName(zone, ms)}`
}

/** `11:23:56.719 PDT`: a log line's time; a value that is not a time is returned as it is. */
export function formatClock(value: string, zone: TimeZone): string {
  const t = parseTime(value)
  if (t === null) return value
  const p = parts(t, zone, { hour: '2-digit', minute: '2-digit', second: '2-digit', fractionalSecondDigits: 3 })
  return `${p.hour}:${p.minute}:${p.second}.${p.fractionalSecond} ${zoneName(zone, t)}`
}

/** `14:00`: an axis tick; the axis names the zone once. */
export function formatHour(ms: number, zone: TimeZone): string {
  const p = parts(ms, zone, { hour: '2-digit', minute: '2-digit' })
  return `${p.hour}:${p.minute}`
}

/** The hour of the day (0–23) at `ms` in the zone. */
export function hourOf(ms: number, zone: TimeZone): number {
  return Number(parts(ms, zone, { hour: '2-digit' }).hour)
}
