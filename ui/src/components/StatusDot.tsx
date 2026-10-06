import type { CSSProperties } from 'react'
import type { StatusKind } from '@/lib/types'
import { statusMeta } from '@/lib/status'

export interface StatusDotProps {
  status: StatusKind
  size?: number
  /** Override the auto pulse (running pulses by default). */
  pulse?: boolean
  style?: CSSProperties
}

/**
 * barca · StatusDot
 * A small state indicator. `running` pulses; everything else is still.
 */
export function StatusDot({ status, size = 8, pulse, style }: StatusDotProps) {
  const meta = statusMeta(status)
  const live = pulse ?? meta.live
  return (
    <span
      style={{
        width: size,
        height: size,
        borderRadius: '50%',
        background: meta.color,
        flex: 'none',
        display: 'inline-block',
        boxShadow: live ? `0 0 8px -1px ${meta.color}` : 'none',
        animation: live
          ? 'barca-pulse var(--dur-pulse) var(--ease-in-out) infinite'
          : 'none',
        ...style,
      }}
    />
  )
}
