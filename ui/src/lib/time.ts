/**
 * The one time format: relative (`3h ago`, `in 5m`), drawn by `Age`, with the absolute time in
 * the reader's chosen zone on hover; absolute in text only where a time is the subject (a log
 * line's clock, an axis tick). The server writes UTC `YYYY-MM-DD HH:MM:SS` (SQLite `datetime('now')`)
 * or RFC 3339; both read the same. Every absolute time names its zone (`UTC`, or the reader's
 * own as the browser abbreviates it: `PDT`, `GMT+1`, `GMT+5:30`), so a screen never mixes zones
 * silently. The zone is a setting (`useTimeZone`), persisted next to the theme; `local` is the
 * default.
 */

export type TimeZone = 'local' | 'utc'


export function isTimeZone(value: unknown): value is TimeZone {
  return value === 'local' || value === 'utc'
}

const DATABASE_TIME = /^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}$/

/** Ms since epoch; null when `value` is not a time. */
export function parseTime(value: string): number | null {
  const t = Date.parse(DATABASE_TIME.test(value) ? `${value.replace(' ', 'T')}Z` : value)
  return Number.isNaN(t) ? null : t
}

/** What an absent time reads as: a time not recorded, or a `now` not known yet. */
export const NO_TIME = '–'

/**
 * Whether `ms` is a time. `0` is not: it is what a time not recorded yet looks like (a query's
 * `dataUpdatedAt` before its first answer, a zeroed timestamp), and shown relative to a real
 * time it would read as fifty-odd years (`in 20774d`).
 */
export function isInstant(ms: number | null | undefined): ms is number {
  return typeof ms === 'number' && Number.isFinite(ms) && ms > 0
}

/** `3h ago`; a value that is not a time is returned as it is, an absent one or an absent `nowMs` as `–`. */
export function formatAgo(value: string, nowMs: number): string {
  const t = parseTime(value)
  if (t === null) return value
  return isInstant(t) && isInstant(nowMs) ? agoOf(t, nowMs) : NO_TIME
}

/** `3h ago` of a time in ms; a time ahead of `nowMs` reads `just now`. */
function agoOf(ms: number, nowMs: number): string {
  const s = Math.max(0, (nowMs - ms) / 1000)
  if (s < 60) return 'just now'
  if (s < 3600) return `${Math.floor(s / 60)}m ago`
  if (s < 86400) return `${Math.floor(s / 3600)}h ago`
  return `${Math.floor(s / 86400)}d ago`
}

/**
 * `in 12s`, `in 12m`, `in 3h 5m`, `in 2d`; a time already past is `due now` (a next run). An
 * absent time, or an absent `nowMs`, is `–`.
 */
export function formatCountdown(atMs: number, nowMs: number): string {
  if (!isInstant(atMs) || !isInstant(nowMs)) return NO_TIME
  const s = Math.floor((atMs - nowMs) / 1000)
  if (s < 0) return 'due now'
  if (s < 60) return `in ${s}s`
  const m = Math.floor(s / 60)
  if (m < 60) return `in ${m}m`
  const h = Math.floor(m / 60)
  if (h < 24) return `in ${h}h ${m % 60}m`
  return `in ${Math.floor(h / 24)}d`
}

/**
 * A time relative to `nowMs`, either way: `3h ago` behind it, `in 5m` ahead of it. An absent
 * time, or an absent `nowMs` (data still loading), is `–`: never an age measured from the epoch.
 */
export function formatRelative(ms: number, nowMs: number): string {
  if (!isInstant(ms) || !isInstant(nowMs)) return NO_TIME
  return ms > nowMs ? formatCountdown(ms, nowMs) : agoOf(ms, nowMs)
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

/** `0.4s`, `12.3s`, `2.1m`, `2.0h`: a measured duration to one decimal of its unit. */
export function formatSeconds(s: number): string {
  if (s < 60) return `${s.toFixed(1)}s`
  if (s < 3600) return `${(s / 60).toFixed(1)}m`
  return `${(s / 3600).toFixed(1)}h`
}

/** `45s`, `12m`, `3h`, `2d`: a length of time in its largest whole unit, as the CLI prints one. */
export function formatSpan(seconds: number): string {
  const s = Math.max(0, Math.floor(seconds))
  if (s < 60) return `${s}s`
  if (s < 3600) return `${Math.floor(s / 60)}m`
  if (s < 86400) return `${Math.floor(s / 3600)}h`
  return `${Math.floor(s / 86400)}d`
}

/**
 * `~15:05`: an estimated time of day, to the minute and rounded up, so an estimate is never
 * said to be sooner than the server gave it; with the day (`~Nov 18 06:00`) when it is not
 * today's in the zone. A value that is not a time is returned as it is.
 */
export function formatEstimate(value: string, zone: TimeZone, nowMs: number): string {
  const t = parseTime(value)
  if (t === null) return value
  return `~${clockOf(Math.ceil(t / 60_000) * 60_000, zone, nowMs)}`
}

/** `15:00`: a time of day to the minute (a tick's slot is exact), with the day when it is not today's. */
export function formatSlot(value: string, zone: TimeZone, nowMs: number): string {
  const t = parseTime(value)
  return t === null ? value : clockOf(t, zone, nowMs)
}

function clockOf(ms: number, zone: TimeZone, nowMs: number): string {
  const p = parts(ms, zone, { month: 'short', day: '2-digit', hour: '2-digit', minute: '2-digit' })
  const today = parts(nowMs, zone, { month: 'short', day: '2-digit' })
  const clock = `${p.hour}:${p.minute}`
  return p.month === today.month && p.day === today.day ? clock : `${p.month} ${p.day} ${clock}`
}

/**
 * `~40s`, `~3m`, `~2h`: a length of time that is an estimate, rounded up to a unit that does
 * not claim more than it knows: 5 seconds under a minute (`<5s` below that), whole minutes
 * under an hour, then whole hours and days.
 */
export function formatAbout(seconds: number): string {
  const s = Math.max(0, seconds)
  if (s < 5) return '<5s'
  if (s < 60) return `~${Math.ceil(s / 5) * 5}s`
  if (s < 3600) return `~${Math.ceil(s / 60)}m`
  if (s < 86400) return `~${Math.ceil(s / 3600)}h`
  return `~${Math.ceil(s / 86400)}d`
}
