import type { CSSProperties } from 'react'
import type { Tone } from '@/lib/tone'

export interface StatusDotProps {
  tone: Tone
  size?: number
  /** Pulse (live work). */
  live?: boolean
  /** What the dot stands for, for a screen reader; without one the dot is decoration. */
  label?: string
  style?: CSSProperties
}

/**
 * barca · StatusDot
 * A small indicator in a tone. Colour is never the only carrier: give it a `label`, or put the
 * word beside it. `live` pulses.
 */
export function StatusDot({ tone, size = 8, live = false, label, style }: StatusDotProps) {
  return (
    <span
      className="barca-dot"
      data-tone={tone}
      data-live={live || undefined}
      role={label ? 'img' : undefined}
      aria-label={label}
      aria-hidden={label ? undefined : true}
      style={{ width: size, height: size, ...style }}
    />
  )
}
