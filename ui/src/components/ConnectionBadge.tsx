import { StatusDot } from './StatusDot'
import { connectionLabel, connectionStatus, type Connection } from '@/lib/connection'

export interface ConnectionBadgeProps {
  connection: Connection
  /** Text for an unreachable server, when the page has more to say than "offline". */
  offlineLabel?: string
  /** Replaces the default text for an online server (e.g. the scheduler's state). */
  onlineLabel?: string
}

/**
 * barca · ConnectionBadge
 * The connection chip in a page's top bar. Its width is reserved (see `.barca-conn`) so
 * the text changing between connecting, online and offline never moves its neighbours.
 */
export function ConnectionBadge({ connection, offlineLabel, onlineLabel }: ConnectionBadgeProps) {
  const text =
    connection.kind === 'online' && onlineLabel ? onlineLabel : connectionLabel(connection, offlineLabel)
  return (
    <span className="barca-conn">
      <StatusDot status={connectionStatus(connection)} size={6} />
      {text}
    </span>
  )
}
