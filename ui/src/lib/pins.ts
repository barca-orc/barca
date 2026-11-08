/**
 * Pinned assets (`GET /pins`, `barca docs pins`): what a pinned node shows instead of its cache
 * state. A pinned asset is read by every run as its pinned version and never recomputed, so
 * `stale` would mislead: it reads `pinned`, with what it is pinned to. Pure.
 */

import type { AssetPin } from './types'

/** `GET /pins`. */
export interface Pins {
  pins: AssetPin[]
}

/** The short form of a run id or hash. */
function short(id: string): string {
  return id.slice(0, 12)
}

/**
 * What a pin holds, in a line: `override from fix.json` for an upload, `rolled back to <run>`
 * for an earlier version, then `by X` when known.
 */
export function pinNote(pin: AssetPin): string {
  const what = pin.upload
    ? `override${pin.upload.file_name ? ` from ${pin.upload.file_name}` : ''}`
    : `rolled back to ${pin.run_id ? short(pin.run_id) : short(pin.run_hash)}`
  return pin.pinned_by ? `${what} by ${pin.pinned_by}` : what
}

/** Each pinned node id and its note. */
export function pinNotes(pins: Pins | undefined): ReadonlyMap<string, string> {
  return new Map((pins?.pins ?? []).map((p) => [p.node_id, pinNote(p)]))
}

/** The banner's words; null when nothing is pinned. */
export function pinnedBanner(total: number): { headline: string; detail: string } | null {
  if (total <= 0) return null
  return {
    headline: total === 1 ? '1 asset pinned' : `${total} assets pinned`,
    detail: '· runs read pinned versions',
  }
}

/** The formats an upload may be, by extension (`barca docs pins`). */
export const UPLOAD_EXTENSIONS = ['.json', '.parquet', '.csv', '.pkl', '.pickle']

/** The format a file name's extension names, or null. */
export function uploadFormat(fileName: string): string | null {
  const ext = fileName.slice(fileName.lastIndexOf('.')).toLowerCase()
  if (!fileName.includes('.') || !UPLOAD_EXTENSIONS.includes(ext)) return null
  return ext === '.pkl' || ext === '.pickle' ? 'pickle' : ext.slice(1)
}
