import type { CSSProperties, ReactNode } from 'react'
import { Ban, CircleX, Pin } from 'lucide-react'
import { LAST_ATTEMPT_FAILED } from '@/lib/cacheStateDisplay'
import type { Tone } from '@/lib/tone'
import { StatusDot } from './StatusDot'

export interface DagNodeProps {
  name: string
  kind: string
  /** The word for its kind; none for an asset, which its icon says already. */
  kindLabel?: string
  /** The colour of its state. */
  tone: Tone
  /** The word: its cache state as the CLI prints it, or a run's status. */
  label?: string | null
  /** Its last attempt failed: a second fact, beside the state. */
  lastFailed?: boolean
  /** The failed node upstream that keeps it from running: drawn with the `blocked` icon. */
  blockedBy?: string
  /** Pinned (`barca pin`): what it is pinned to; drawn with the pin icon. */
  pinned?: string
  /** A run is working on it: the live border and pulse. */
  live?: boolean
  stateHint?: string
  selected?: boolean
  icon?: ReactNode
  style?: CSSProperties
  onClick?: () => void
}

export const DAG_NODE_WIDTH = 208
export const DAG_NODE_HEIGHT = 64

/**
 * barca · DagNode
 * The signature object: one node in a pipeline graph. The tone drives the accent; the word is
 * always visible. `selected` rings it in the neutral signal; a live node gets a cyan border.
 */
export function DagNode({
  name,
  kind,
  kindLabel,
  tone,
  label,
  lastFailed = false,
  blockedBy,
  pinned,
  live = false,
  stateHint,
  selected = false,
  icon,
  style,
  onClick,
}: DagNodeProps) {
  return (
    <div
      className="barca-dag"
      onClick={onClick}
      title={stateHint ?? label ?? undefined}
      data-tone={tone}
      data-kind={kind}
      data-live={live || undefined}
      data-selected={selected || undefined}
      data-failed={lastFailed || undefined}
      data-clickable={onClick ? '' : undefined}
      style={style}
    >
      <span className="barca-dag-spine" />
      <div className="barca-dag-body">
        <div className="barca-dag-head">
          <StatusDot tone={tone} live={live} size={7} />
          <span className="barca-dag-name">{name}</span>
          {icon && <span className="barca-dag-icon">{icon}</span>}
        </div>
        <div className="barca-dag-foot">
          {/* The kind's word gives way to the failed fact: the icon above still says the kind. */}
          {kindLabel && !lastFailed && <span className="barca-dag-kind">{kindLabel}</span>}
          {/* One state per node: a failed last attempt is `failing`, in red with its icon, instead of the cache word. */}
          {(label || lastFailed) && (
            <span className="barca-dag-state" data-failed={lastFailed || undefined} data-blocked={(!lastFailed && blockedBy) || undefined} data-pinned={(!lastFailed && pinned !== undefined) || undefined}>
              {lastFailed && <CircleX size={11} aria-hidden />}
              {!lastFailed && pinned !== undefined && <Pin size={11} aria-hidden />}
              {!lastFailed && pinned === undefined && blockedBy && <Ban size={11} aria-hidden />}
              {lastFailed ? LAST_ATTEMPT_FAILED : label}
            </span>
          )}
        </div>
      </div>
    </div>
  )
}
