import type { CSSProperties } from 'react'
import { Ban, CircleAlert, CircleX, Clock, LoaderCircle, Pause } from 'lucide-react'
import type { Tone } from '@/lib/tone'
import { StatusDot } from './StatusDot'

export interface StatusBadgeProps {
  tone: Tone
  /** The words: a node's state (`failing`, `never run`) or a run's status. */
  label: string
  size?: 'sm' | 'md'
  /** Pulse (live work). */
  live?: boolean
  /** Dot + plain text, no pill. By default only the failed and running tones, the exceptions, get a pill. */
  subtle?: boolean
  /** A shape for a state the tone does not name (`paused` is neutral in colour). */
  icon?: 'paused' | 'blocked'
  style?: CSSProperties
}

/**
 * The shape beside the word, so a state is never colour alone: failed is a crossed circle,
 * a warning (stale) an exclamation, blocked by a failure upstream a barred circle, running a spinner, queued a clock, paused bars.
 * The quiet tones (fresh, never run) keep a dot.
 */
function StateIcon({ tone, live, size, icon }: { tone: Tone; live: boolean; size: number; icon?: 'paused' | 'blocked' }) {
  const props = { size, 'aria-hidden': true, strokeWidth: 2.25, className: 'barca-badge-icon' } as const
  if (icon === 'paused') return <Pause {...props} />
  if (icon === 'blocked') return <Ban {...props} />
  if (tone === 'failed') return <CircleX {...props} />
  if (tone === 'warning') return <CircleAlert {...props} />
  if (tone === 'running') return <LoaderCircle {...props} data-spin={live || undefined} />
  if (tone === 'queued') return <Clock {...props} />
  return <StatusDot tone={tone} live={live} size={size === 11 ? 6 : 7} />
}

/**
 * barca · StatusBadge
 * An icon and its word. Pills are for exceptions: a failed or running state sits on a tinted
 * ground, any other is icon + plain text, so a column of fresh rows stays quiet.
 */
export function StatusBadge({ tone, label, size = 'md', live = false, subtle, icon, style }: StatusBadgeProps) {
  const plain = subtle ?? !(tone === 'failed' || tone === 'running')
  return (
    <span className="barca-badge" data-tone={tone} data-size={size} data-plain={plain || undefined} style={style}>
      <StateIcon tone={tone} live={live} size={size === 'sm' ? 11 : 13} icon={icon} />
      {label}
    </span>
  )
}
