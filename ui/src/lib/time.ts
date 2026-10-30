/**
 * The one time format: relative where it is read at a glance, absolute UTC on hover. The server
 * writes UTC `YYYY-MM-DD HH:MM:SS` (SQLite `datetime('now')`) or RFC 3339; both read the same.
 */

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

/** `2026-10-30 00:35:45 UTC`; a value that is not a time is returned as it is. */
export function formatAbsolute(value: string): string {
  const t = parseTime(value)
  return t === null ? value : `${new Date(t).toISOString().slice(0, 19).replace('T', ' ')} UTC`
}
