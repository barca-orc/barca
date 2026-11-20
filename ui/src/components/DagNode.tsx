import { compactCount } from '@/lib/plural'
import type { CSSProperties } from 'react'
import type { MapPart } from '@/lib/causeDisplay'
import { toneGlyph, type Glyph } from '@/lib/glyph'
import type { Tone } from '@/lib/tone'
import { CauseDot } from './CauseDot'
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
  /** The word: its cause as the health map names it (or `fresh`), or a run's status. */
  label?: string | null
  /** At rest: the cause (or fresh) the word is of, drawn as a dot in the health map's colour. */
  part?: MapPart
  /** Its last attempt failed. */
  lastFailed?: boolean
  /** Pinned (`barca pin`): what it is pinned to; it reads `pinned`. */
  pinned?: string
  /** A run is working on it: the live border, and the running glyph turns. */
  live?: boolean
  stateHint?: string
  /** A partitioned node's keys: a short count at the end, and a bar along the bottom of the share failed and waiting. */
  keys?: { total: number; failed: number; waiting: number }
  selected?: boolean
  style?: CSSProperties
  onClick?: () => void
}

/** The glyph a node shows: pinned, then its state's own. */
function nodeGlyph({ glyph, tone, pinned }: Pick<DagNodeProps, 'glyph' | 'tone' | 'pinned'>): Glyph {
  if (pinned !== undefined) return 'pinned'
  return glyph ?? toneGlyph(tone)
}

/**
 * barca · DagNode
 * One node in a pipeline graph, on one line: its state's mark and its name, and for a task or
 * a sensor its kind, muted at the end. At rest the mark is a dot in the colour the health map
 * gives the node's cause (or fresh); a run working on it draws the run's glyph, a pinned node the
 * pin. The word is not drawn again under the name: the legend names each colour, and the card's
 * tooltip and accessible name carry the word. The card itself stays neutral whatever the state:
 * the mark is the only colour. `selected` rings it; a live node gets the running ring.
 */
export function DagNode({
  name,
  kind,
  kindLabel,
  tone,
  glyph,
  label,
  part,
  lastFailed = false,
  pinned,
  live = false,
  stateHint,
  keys,
  selected = false,
  style,
  onClick,
}: DagNodeProps) {
  const shown = nodeGlyph({ glyph, tone, pinned })
  const word = label
  // A dot in the map's colour at rest; the glyph for run work and a pin.
  const dot = part !== undefined && !live && pinned === undefined ? part : null
  return (
    <div
      className="barca-dag"
      onClick={onClick}
      title={[word, stateHint].filter(Boolean).join(' · ') || undefined}
      data-tone={tone}
      data-glyph={shown}
      data-cause={dot ?? undefined}
      data-kind={kind}
      data-live={live || undefined}
      data-selected={selected || undefined}
      data-failed={lastFailed || undefined}
      data-clickable={onClick ? '' : undefined}
      style={style}
    >
      <div className="barca-dag-body">
        {dot ? <CauseDot part={dot} size={10} /> : <StatusGlyph glyph={shown} live={live} size={14} />}
        <span className="barca-dag-name">{name}</span>
        {kindLabel && <span className="barca-dag-kind">{kindLabel}</span>}
        {keys && <span className="barca-dag-kind" title={keysTitle(keys)}>{compactCount(keys.total)} keys</span>}
        {/* The state's word, for a screen reader and a search of the page: the glyph shows it. */}
        {word && (
          <span className="barca-dag-state barca-sr-only" data-failed={lastFailed || undefined} data-pinned={pinned !== undefined || undefined}>
            {word}
          </span>
        )}
      </div>
      {keys && (keys.failed > 0 || keys.waiting > 0) && (
        // A sliver of a few keys in thousands still shows: at least 3px each.
        <div className="barca-dag-keys" aria-hidden>
          {keys.failed > 0 && <span className="is-failed" style={{ width: `max(3px, ${(100 * keys.failed) / keys.total}%)` }} />}
          {keys.waiting > 0 && <span className="is-waiting" style={{ width: `max(3px, ${(100 * keys.waiting) / keys.total}%)` }} />}
        </div>
      )}
    </div>
  )
}

function keysTitle(keys: { total: number; failed: number; waiting: number }): string {
  const parts = [`${keys.total.toLocaleString()} keys`]
  if (keys.failed > 0) parts.push(`${keys.failed.toLocaleString()} failed`)
  if (keys.waiting > 0) parts.push(`${keys.waiting.toLocaleString()} waiting to run`)
  return parts.join(' · ')
}
