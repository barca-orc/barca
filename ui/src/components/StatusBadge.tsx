import type { CSSProperties } from 'react'
import type { StatusKind } from '@/lib/types'
import { statusMeta } from '@/lib/status'
import { StatusDot } from './StatusDot'

export interface StatusBadgeProps {
  status: StatusKind
  label?: string
  size?: 'sm' | 'md'
  /** Dot + plain text, no pill. By default only failed and running, the exceptions, get a pill. */
  subtle?: boolean
  style?: CSSProperties
}

const SIZES = {
  sm: { height: 18, padding: '0 7px 0 6px', fontSize: 'var(--text-2xs)', dot: 6, gap: 5 },
  md: { height: 22, padding: '0 9px 0 8px', fontSize: 'var(--text-xs)', dot: 7, gap: 6 },
} as const

/**
 * barca · StatusBadge
 * A run/asset state: a colored dot + label. Pills are for exceptions: a failed or running
 * state sits on a tinted ground, any other is dot + plain text, so a column of healthy rows
 * stays quiet.
 */
export function StatusBadge({
  status,
  label,
  size = 'md',
  subtle,
  style,
}: StatusBadgeProps) {
  const meta = statusMeta(status)
  const s = SIZES[size]
  const plain = subtle ?? !(status === 'failed' || status === 'running')
  return (
    <span
      style={{
        display: 'inline-flex',
        alignItems: 'center',
        gap: s.gap,
        height: s.height,
        padding: s.padding,
        borderRadius: 'var(--radius-pill)',
        background: plain ? 'transparent' : meta.bg,
        border: `1px solid ${plain ? 'transparent' : meta.line}`,
        color: plain ? 'var(--text-default)' : meta.color,
        fontFamily: 'var(--font-sans)',
        fontSize: s.fontSize,
        fontWeight: 500,
        letterSpacing: '0.01em',
        whiteSpace: 'nowrap',
        ...style,
      }}
    >
      <StatusDot status={status} size={s.dot} />
      {label ?? meta.label}
    </span>
  )
}
