import { StatusDot } from './StatusDot'
import { connectionLabel, connectionStatus, type Connection } from '@/lib/connection'

export interface ConnectionBadgeProps {
  connection: Connection
}

/**
 * barca · ConnectionBadge
 * The connection in the top bar: a 12px dot you can hover or tab to for its text (`.barca-conn`
 * in shell.css shows the label then). A healthy server is just the dot, its version in the
 * label; a connection that is not up says so in words all the time.
 */
export function ConnectionBadge({ connection }: ConnectionBadgeProps) {
  const text = connectionLabel(connection)
  return (
    <span className="barca-conn" data-state={connection.kind} title={text} tabIndex={0}>
      <StatusDot status={connectionStatus(connection)} size={12} />
      <span className="barca-conn-label">{text}</span>
    </span>
  )
}
