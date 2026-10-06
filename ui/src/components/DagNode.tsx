import type { CSSProperties, ReactNode } from 'react'
import type { StatusKind } from '@/lib/types'
import { statusMeta } from '@/lib/status'
import { StatusDot } from './StatusDot'

export interface DagNodeProps {
  name: string
  kind: string
  status: StatusKind
  metric?: string | null
  selected?: boolean
  icon?: ReactNode
  style?: CSSProperties
  onClick?: () => void
}

export const DAG_NODE_WIDTH = 208
export const DAG_NODE_HEIGHT = 64

/**
 * barca · DagNode
 * The signature object: one node in a pipeline graph. Status drives the accent.
 * `selected` rings it in signal green; `running` gets a live cyan border + spine.
 */
export function DagNode({
  name,
  kind,
  status,
  metric,
  selected = false,
  icon,
  style,
  onClick,
}: DagNodeProps) {
  const meta = statusMeta(status)
  const running = status === 'running'

  const borderColor = selected
    ? 'var(--signal)'
    : running
      ? 'var(--status-running-line)'
      : 'var(--border-strong)'

  const ring = selected
    ? '0 0 0 1px var(--signal), 0 0 18px -4px var(--c-green-glow)'
    : running
      ? '0 0 14px -5px rgba(54,203,211,0.6)'
      : 'var(--shadow-sm)'

  // Tasks always re-run (never cached) — a dashed border marks that ephemeral
  // quality; assets (cached data) stay solid.
  const borderStyle = kind === 'task' ? 'dashed' : 'solid'

  return (
    <div
      onClick={onClick}
      data-status={status}
      data-kind={kind}
      style={{
        position: 'relative',
        width: DAG_NODE_WIDTH,
        background: 'var(--bg-raised)',
        border: `1px ${borderStyle} ${borderColor}`,
        borderRadius: 'var(--radius-md)',
        boxShadow: `${ring}, var(--edge-top)`,
        cursor: onClick ? 'pointer' : 'default',
        transition: 'var(--t-all)',
        ...style,
      }}
    >
      {/* left status spine */}
      <span
        style={{
          position: 'absolute',
          left: 0,
          top: 8,
          bottom: 8,
          width: 2,
          borderRadius: 2,
          background: meta.color,
          animation: running
            ? 'barca-pulse var(--dur-pulse) var(--ease-in-out) infinite'
            : 'none',
        }}
      />

      <div style={{ padding: '9px 12px 10px' }}>
        <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
          <StatusDot status={status} size={7} />
          <span
            style={{
              flex: 1,
              minWidth: 0,
              fontFamily: 'var(--font-mono)',
              fontSize: 'var(--text-sm)',
              fontWeight: 500,
              color: 'var(--text-strong)',
              letterSpacing: '-0.01em',
              whiteSpace: 'nowrap',
              overflow: 'hidden',
              textOverflow: 'ellipsis',
            }}
          >
            {name}
          </span>
          {icon && (
            <span style={{ color: 'var(--text-faint)', display: 'inline-flex', flex: 'none' }}>
              {icon}
            </span>
          )}
        </div>
        <div
          style={{
            display: 'flex',
            alignItems: 'center',
            justifyContent: 'space-between',
            gap: 8,
            marginTop: 6,
          }}
        >
          <span
            style={{
              fontFamily: 'var(--font-mono)',
              fontSize: 'var(--text-2xs)',
              letterSpacing: 'var(--tracking-wide)',
              textTransform: 'uppercase',
              color: 'var(--text-faint)',
            }}
          >
            {kind}
          </span>
          {metric && (
            <span
              style={{
                fontFamily: 'var(--font-mono)',
                fontSize: 'var(--text-2xs)',
                color: running ? 'var(--status-running)' : 'var(--text-muted)',
              }}
            >
              {metric}
            </span>
          )}
        </div>
      </div>
    </div>
  )
}
