import { StatusDot } from './StatusDot'
import { connectionLabel, connectionStatus, type Connection } from '@/lib/connection'

export interface ConnectionBadgeProps {
  connection: Connection
}

/**
 * barca · ConnectionBadge
 * The connection in the top bar. A healthy server is a dot, its version in the tooltip; only a
 * connection that is not up says so in words.
 */
export function ConnectionBadge({ connection }: ConnectionBadgeProps) {
  const text = connectionLabel(connection)
  return (
    <span className="barca-conn" title={text}>
      <StatusDot status={connectionStatus(connection)} size={6} />
      <span className={connection.kind === 'online' ? 'barca-sr-only' : undefined}>{text}</span>
    </span>
  )
}
