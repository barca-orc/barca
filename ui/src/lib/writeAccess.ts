/**
 * Whether this server takes writes, and so whether the UI offers them: the one decision every
 * write action reads (run, run all, re-run, retry, cancel, pause and resume, pin, upload,
 * unpin, reset). A `barca serve --read-only` refuses them all (403), so on one they are not
 * there at all, and the app says once, quietly, that the server is read-only
 * (`ReadOnlyNotice`), instead of a disabled button with a tooltip at every turn.
 */

import { match } from 'ts-pattern'
import type { Health } from './types'

export type WriteAccess =
  /** `/health` has not answered yet. */
  | { kind: 'checking' }
  /** `barca serve --read-only`. */
  | { kind: 'read_only' }
  | { kind: 'writable' }

export function writeAccess(health: Pick<Health, 'read_only'> | undefined): WriteAccess {
  if (!health) return { kind: 'checking' }
  return health.read_only ? { kind: 'read_only' } : { kind: 'writable' }
}

/**
 * How a write action is drawn: not at all on a read-only server; there but disabled until it
 * is known (a toolbar does not move when `/health` answers on a server that takes writes, the
 * usual case); enabled.
 */
export type WriteControl = 'hidden' | 'disabled' | 'enabled'

export function writeControl(access: WriteAccess): WriteControl {
  return match(access)
    .with({ kind: 'checking' }, (): WriteControl => 'disabled')
    .with({ kind: 'read_only' }, (): WriteControl => 'hidden')
    .with({ kind: 'writable' }, (): WriteControl => 'enabled')
    .exhaustive()
}

/** The app's one read-only line: what it is, and what it means. */
export const READ_ONLY_LABEL = 'Read-only'
export const READ_ONLY_NOTE = 'This server was started with --read-only: it shows state and history; runs, retries, pauses and pins are made elsewhere.'
