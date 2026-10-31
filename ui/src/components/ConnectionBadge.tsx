import { StatusDot } from './StatusDot'
import { connectionLabel, connectionStatus, type Connection } from '@/lib/connection'

export interface ConnectionBadgeProps {
  connection: Connection
}

/**
 * barca · ConnectionBadge
 * The connection in the top bar: a 12px dot you can hover or tab to for its text (`.barca-conn`
 * in shell.css shows the label then). A healthy server is just the dot, its version in the
 * label; a connection that is not up says so in words all the time, and an offline one in an
 * amber pill no one has to look for (the banner above the page says what it means).
 */
export function ConnectionBadge({ connection }: ConnectionBadgeProps) {
  const text = connectionLabel(connection)
  const offline = connection.kind === 'offline'
  return (
    <span
      className="barca-conn"
      data-state={connection.kind}
      title={text}
      tabIndex={0}
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
      <StatusDot status={offline ? 'warning' : connectionStatus(connection)} size={12} />
      <span className="barca-conn-label">{text}</span>
    </span>
  )
}
