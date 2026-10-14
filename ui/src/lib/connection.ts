/**
 * Connection to `barca serve`, as the top bars show it. Three states, not two:
 * before the first health response the server is neither up nor down, and showing
 * "offline" for that moment is both wrong and a layout shift when the real state lands.
 */

import { match } from 'ts-pattern'
import type { Health, StatusKind } from './types'

export type Connection =
  | { kind: 'connecting' }
  | { kind: 'online'; version: string }
  | { kind: 'offline' }

export function connection(health: Health | undefined, isError: boolean): Connection {
  if (health && !isError) return { kind: 'online', version: health.version }
  return isError ? { kind: 'offline' } : { kind: 'connecting' }
}

export function connectionStatus(c: Connection): StatusKind {
  return match(c)
    .with({ kind: 'online' }, () => 'success' as const)
    .with({ kind: 'connecting' }, { kind: 'offline' }, () => 'queued' as const)
    .exhaustive()
}

/** `offline` is the text for a server that did not answer (pages say what that means for them). */
export function connectionLabel(c: Connection, offline = 'offline'): string {
  return match(c)
    .with({ kind: 'online' }, (o) => `barca serve · v${o.version}`)
    .with({ kind: 'connecting' }, () => 'connecting…')
    .with({ kind: 'offline' }, () => offline)
    .exhaustive()
}
