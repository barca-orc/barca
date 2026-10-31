import { StatusDot } from './StatusDot'
import { connectionLabel, connectionStatus, type Connection } from '@/lib/connection'

export interface ConnectionBadgeProps {
  connection: Connection
}

/**
 * barca · ConnectionBadge
 * The connection in the top bar. A healthy server is a dot, its version in the tooltip; a
 * connection that is not up says so in words, and an offline one in an amber pill no one
 * has to look for (the banner above the page says what it means).
 */
export function ConnectionBadge({ connection }: ConnectionBadgeProps) {
  const text = connectionLabel(connection)
  const offline = connection.kind === 'offline'
  return (
    <span
      className="barca-conn"
      title={text}
      style={
        offline
          ? {
              padding: '2px 8px',
              borderRadius: 'var(--radius-sm)',
              border: '1px solid var(--status-warning-line)',
              background: 'var(--status-warning-bg)',
              color: 'var(--status-warning)',
              fontWeight: 600,
            }
          : undefined
      }
    >
      <StatusDot status={offline ? 'warning' : connectionStatus(connection)} size={6} />
      <span className={connection.kind === 'online' ? 'barca-sr-only' : undefined}>{text}</span>
    </span>
  )
}
