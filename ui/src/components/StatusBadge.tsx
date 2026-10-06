import type { CSSProperties } from 'react'
import type { StatusKind } from '@/lib/types'
import { statusMeta } from '@/lib/status'
import { StatusDot } from './StatusDot'

export interface StatusBadgeProps {
  status: StatusKind
  label?: string
  size?: 'sm' | 'md'
  /** Drop the tinted ground + border, keep just the dot + colored label. */
  subtle?: boolean
  style?: CSSProperties
}

const SIZES = {
  sm: { height: 18, padding: '0 7px 0 6px', fontSize: 'var(--text-2xs)', dot: 6, gap: 5 },
  md: { height: 22, padding: '0 9px 0 8px', fontSize: 'var(--text-xs)', dot: 7, gap: 6 },
} as const

/**
 * barca · StatusBadge
 * Pill showing a run/asset state: a colored dot + mono label on a faint
 * tinted ground. The status vocabulary of the whole product.
 */
export function StatusBadge({
  status,
  label,
  size = 'md',
  subtle = false,
  style,
}: StatusBadgeProps) {
  const meta = statusMeta(status)
  const s = SIZES[size]
  return (
    <span
      style={{
        display: 'inline-flex',
        alignItems: 'center',
        gap: s.gap,
        height: s.height,
        padding: s.padding,
        borderRadius: 'var(--radius-pill)',
        background: subtle ? 'transparent' : meta.bg,
        border: `1px solid ${subtle ? 'transparent' : meta.line}`,
        color: meta.color,
        fontFamily: 'var(--font-mono)',
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
