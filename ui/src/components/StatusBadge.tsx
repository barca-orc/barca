import type { CSSProperties } from 'react'
import type { Tone } from '@/lib/tone'
import { StatusDot } from './StatusDot'

export interface StatusBadgeProps {
  tone: Tone
  /** The word: a cache state as the CLI prints it, or a run's status. */
  label: string
  size?: 'sm' | 'md'
  /** Pulse (live work). */
  live?: boolean
  /** Dot + plain text, no pill. By default only the failed and running tones, the exceptions, get a pill. */
  subtle?: boolean
  style?: CSSProperties
}

/**
 * barca · StatusBadge
 * A dot and its word. Pills are for exceptions: a failed or running state sits on a tinted
 * ground, any other is dot + plain text, so a column of cached rows stays quiet.
 */
export function StatusBadge({ tone, label, size = 'md', live = false, subtle, style }: StatusBadgeProps) {
  const plain = subtle ?? !(tone === 'failed' || tone === 'running')
  return (
    <span className="barca-badge" data-tone={tone} data-size={size} data-plain={plain || undefined} style={style}>
      <StatusDot tone={tone} live={live} size={size === 'sm' ? 6 : 7} />
      {label}
    </span>
  )
}
