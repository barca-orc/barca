import type { CSSProperties } from 'react'
import { LAST_ATTEMPT_FAILED } from '@/lib/cacheStateDisplay'
import { toneGlyph, type Glyph } from '@/lib/glyph'
import type { Tone } from '@/lib/tone'
import { StatusGlyph } from './StatusGlyph'

export interface DagNodeProps {
  name: string
  kind: string
  /** The word for its kind; none for an asset, the default kind. */
  kindLabel?: string
  /** How urgent its state is. */
  tone: Tone
  /** How its state is drawn; by default the tone's plain glyph. */
  glyph?: Glyph
  /** The word: its cache state as the CLI prints it, or a run's status. */
  label?: string | null
  /** Its last attempt failed: the one state shown, `failing`. */
  lastFailed?: boolean
  /** The failed node upstream that keeps it from running: it reads `blocked`. */
  blockedBy?: string
  /** Pinned (`barca pin`): what it is pinned to; it reads `pinned`. */
  pinned?: string
  /** A run is working on it: the live border, and the running glyph turns. */
  live?: boolean
  stateHint?: string
  selected?: boolean
  style?: CSSProperties
  onClick?: () => void
}

export const DAG_NODE_WIDTH = 208
export const DAG_NODE_HEIGHT = 40

/** The glyph a node shows: failing, pinned, blocked, then its state's own. */
function nodeGlyph({ glyph, tone, lastFailed, blockedBy, pinned }: Pick<DagNodeProps, 'glyph' | 'tone' | 'lastFailed' | 'blockedBy' | 'pinned'>): Glyph {
  if (lastFailed) return 'failed'
  if (pinned !== undefined) return 'pinned'
  if (blockedBy) return 'blocked'
  return glyph ?? toneGlyph(tone)
}

/**
 * barca · DagNode
 * One node in a pipeline graph, on one line: its state's glyph and its name, and for a task or
 * a sensor its kind, muted at the end. The state's word is not drawn again under the name: the
 * glyph says it, and the card's tooltip and accessible name carry the word. The card itself
 * stays neutral whatever the state: the glyph is the only colour. `selected` rings it; a live
 * node gets the running ring.
 */
export function DagNode({
  name,
  kind,
  kindLabel,
  tone,
  glyph,
  label,
  lastFailed = false,
  blockedBy,
  pinned,
  live = false,
  stateHint,
  selected = false,
  style,
  onClick,
}: DagNodeProps) {
  const shown = nodeGlyph({ glyph, tone, lastFailed, blockedBy, pinned })
  const word = lastFailed ? LAST_ATTEMPT_FAILED : label
  return (
    <div
      className="barca-dag"
      onClick={onClick}
      title={[word, stateHint].filter(Boolean).join(' · ') || undefined}
      data-tone={tone}
      data-glyph={shown}
      data-kind={kind}
      data-live={live || undefined}
      data-selected={selected || undefined}
      data-failed={lastFailed || undefined}
      data-clickable={onClick ? '' : undefined}
      style={style}
    >
      <div className="barca-dag-body">
        <StatusGlyph glyph={shown} live={live} size={14} />
        <span className="barca-dag-name">{name}</span>
        {kindLabel && <span className="barca-dag-kind">{kindLabel}</span>}
        {/* The state's word, for a screen reader and a search of the page: the glyph shows it. */}
        {word && (
          <span className="barca-dag-state barca-sr-only" data-failed={lastFailed || undefined} data-blocked={(!lastFailed && blockedBy) || undefined} data-pinned={(!lastFailed && pinned !== undefined) || undefined}>
            {word}
          </span>
        )}
      </div>
    </div>
  )
}
