import { StatusDot } from './StatusDot'
import { connectionLabel, connectionTone, type Connection } from '@/lib/connection'

export interface ConnectionBadgeProps {
  connection: Connection
}

/**
 * barca · ConnectionBadge
 * The connection in the top bar: a dot and a word, never colour alone. A healthy server says
 * `Connected` (its version in the tooltip); an offline one is an amber pill no one has to look
 * for (the banner above the page says what it means).
 */
export function ConnectionBadge({ connection }: ConnectionBadgeProps) {
  const text = connectionLabel(connection)
  const offline = connection.kind === 'offline'
  // Always a word beside the dot: `Connected` (the version in the tooltip), `connecting…`, `offline`.
  const word = connection.kind === 'online' ? 'Connected' : text
  return (
    <span className="barca-conn" data-state={connection.kind} title={text} aria-label={`Server: ${text}`} role="status">
      <StatusDot tone={offline ? 'warning' : connectionTone(connection)} size={8} />
      <span className="barca-conn-word">{word}</span>
    </span>
  )
}
