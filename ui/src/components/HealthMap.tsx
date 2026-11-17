import { useMemo, useState } from 'react'
import { arcPath, freshWithin, noEstimate, ringRadii, sunburst, treePath, ROOT, type Arc, type RollupEntry, type RollupTree } from '@/lib/rollupView'
import { MAP_PARTS, causeLine, causeParts, mapPartColor, mapPartLabel, mapPartMeaning, type MapPart } from '@/lib/causeDisplay'
import { plural } from '@/lib/plural'

export interface HealthMapProps {
  /** The rollup (`GET /state/rollup`): every count the map shows is an entry's own. */
  tree: RollupTree
  /** The scope at the centre: the root, a group or a file. */
  focus: string
  /** Zoom to a scope (a segment with members, or the centre to go up). */
  onFocus: (id: string) => void
  /** Open a node (a leaf segment). */
  onOpen: (id: string) => void
  /**
   * Entries to light while nothing is under the pointer (what a selected root holds, by entry
   * and node id); the rest dims. Null lights everything.
   */
  lit?: ReadonlySet<string> | null
  /** Side of the square, in px (the drawing scales with it). */
  size?: number
}

/** How many levels below the centre are drawn: a branch ends where its members do. */
const MAX_RINGS = 6

/** How many causes the centre lists: the ones that alarm come first in the served order. */
const CENTER_ROWS = 3

/**
 * Geometry, as fractions of the radius. A hole wide enough for three short lines; rings that
 * thin out by `DECAY` each level, so the deep levels read as fine texture and the near ones as
 * the shape; hairline gaps (px, at the drawing's size) between rings and between neighbours.
 */
const HOLE = 0.4
const RING_SHAPE = { firstMax: 0.22, decay: 0.8, gap: 0.8 }
const SEGMENT_GAP = 0.8

/**
 * A band's colour: its cause's (or fresh), a step lighter and a touch softer at each level out
 * (same hue, so a colour stays itself), the way a disk map's deep folders read paler than their
 * parents.
 */
function fillOf(part: MapPart, depth: number): string {
  const k = depth - 1
  const base = mapPartColor(part)
  return k === 0 ? base : `oklch(from ${base} calc(l + ${(0.025 * k).toFixed(3)}) calc(c * ${(1 - 0.05 * k).toFixed(2)}) h)`
}

/** The arc ids lit while `hover` is under the pointer: it, what is inside it, and what holds it. */
function hoverIds(arcs: readonly Arc[], hover: string | null): Set<string> | null {
  if (hover === null) return null
  const byId = new Map(arcs.map((a) => [a.id, a]))
  const lit = new Set<string>([hover])
  for (let at = byId.get(hover); at && byId.has(at.parent); at = byId.get(at.parent)) lit.add(at.parent)
  for (const a of arcs) {
    for (let up: Arc | undefined = a; up; up = byId.get(up.parent)) {
      if (up.parent === hover) {
        lit.add(a.id)
        break
      }
    }
  }
  return lit
}

/** Whether a segment is among `held`: itself, a thin member drawn in it, or the node its keys are of. */
function isHeld(a: Arc, held: ReadonlySet<string>): boolean {
  if (a.kind === 'keys') return held.has(a.parent)
  if (a.kind === 'rest') return (a.members ?? []).some((m) => held.has(m))
  return held.has(a.id)
}

/** A scope in the terms of the gap: `14 overdue · 152 no refresh path`, or that all is fresh. */
function gapWords(entry: RollupEntry): string {
  return causeLine(entry.causes) || 'every node is fresh'
}

/** When what has an estimate is fresh, and how many have none: empty for a fresh scope. */
function estimateWords(entry: RollupEntry, asOf: string): string {
  return [freshWithin(entry, asOf), noEstimate(entry)].filter((part) => part !== null).join(' · ')
}

/**
 * barca · HealthMap
 * The scope as a sunburst, drawn the way a disk map draws a folder: the centre is the scope, each
 * ring one level down (groups, then their groups, then nodes, then a partitioned node's keys),
 * thinner as it goes out, and a branch ends where its members do. A segment's angle is how far
 * what is underneath is from fresh (`mapWeight`: every node counts 1, a node that is not fresh
 * its cause's weight), so the room a group takes is how big its problem is. A segment's colours
 * are what is inside it, stacked: failing red, overdue orange, no refresh path amber, queued
 * and catching up in calm blues, undecided the one grey, fresh green; paler at each level out.
 * Members too thin to draw are one `N smaller` segment; a partitioned node with room shows its
 * keys outside it (failed, not up to date, ok). The centre says the scope in the terms of the
 * gap, in nodes (how many are overdue, have no refresh path, are catching up); the lines under
 * the map name the segment under the pointer, or the scope with when it is expected fresh and
 * how many nodes have no estimate. Click a segment to make it the centre, a node or its keys to
 * open it, the centre to go up.
 */
export function HealthMap({ tree, focus, onFocus, onOpen, lit: held = null, size = 320 }: HealthMapProps) {
  const arcs = useMemo(() => sunburst(tree, focus, MAX_RINGS, 0.008), [tree, focus])
  const [hover, setHover] = useState<string | null>(null)
  const hovering = useMemo(() => hoverIds(arcs, hover), [arcs, hover])
  const center = tree.entries.get(focus)
  if (!center) return null

  const half = size / 2
  const outer = half - 1
  const hole = outer * HOLE
  const rings = ringRadii(arcs.reduce((max, a) => Math.max(max, a.depth), 0), hole, outer, RING_SHAPE)
  const parent = center.parent ?? undefined
  const hovered = hover ? arcs.find((a) => a.id === hover) ?? null : null
  const hoveredEntry = hovered && hovered.kind !== 'rest' && hovered.kind !== 'keys' ? tree.entries.get(hovered.id) : null
  const activate = (a: Arc) => (a.kind === 'node' ? onOpen(a.id) : a.kind === 'keys' ? onOpen(a.parent) : a.kind === 'rest' ? onFocus(a.parent) : onFocus(a.id))
  const isLit = (a: Arc) => (hovering ? hovering.has(a.id) : held ? isHeld(a, held) : true)
  const dimming = hovering !== null || held !== null
  const rows = causeParts(center.causes)

  // The lines under the map: the segment under the pointer, else the scope.
  const name = hovered ? hovered.label : focus === ROOT ? 'All pipelines' : center.label
  // Where the segment sits: the path to it, or to what holds it (`N smaller`, a node's keys).
  const synthetic = hovered?.kind === 'rest' || hovered?.kind === 'keys'
  const where = hovered ? treePath(tree, synthetic ? hovered.parent : hovered.id).slice(1, synthetic ? undefined : -1).map((n) => n.label).join(' › ') : null
  // The scope's own causes are in the centre; a segment's are said here.
  const shown = hoveredEntry ?? (hovered ? null : center)
  const summary = hoveredEntry
    ? `${gapWords(hoveredEntry)} · ${plural(hoveredEntry.nodes, 'node')}`
    : !hovered
      ? `${plural(center.nodes, 'node')}${center.not_fresh > 0 ? `, ${center.not_fresh.toLocaleString()} not fresh` : ', all fresh'}`
      : hovered.keys
        ? `${hovered.keys.count.toLocaleString()} of ${hovered.keys.of.toLocaleString()} keys`
        : 'members too small to draw one by one'
  const estimate = shown ? estimateWords(shown, tree.asOf) : ''
  const describe = (a: Arc): string => {
    const entry = a.kind === 'rest' || a.kind === 'keys' ? undefined : tree.entries.get(a.id)
    if (entry) return `${a.label}: ${gapWords(entry)}, ${plural(entry.nodes, 'node')}`
    return a.keys ? `${a.label}, of ${a.keys.of.toLocaleString()} keys` : a.label
  }

  return (
    <figure className="barca-healthmap" aria-label={`Health map of ${center.label}`}>
      <svg
        width={size}
        height={size}
        viewBox={`${-half} ${-half} ${size} ${size}`}
        role="group"
        data-dimming={dimming || undefined}
        onMouseLeave={() => setHover(null)}
      >
        {arcs.map((a) => {
          const [inner, out] = rings[a.depth - 1]!
          return (
            <g
              key={a.id}
              className="barca-healthmap-arc"
              data-kind={a.kind}
              data-part={a.bands.length === 1 ? a.bands[0]!.part : 'mixed'}
              data-lit={dimming ? isLit(a) : undefined}
              role="button"
              tabIndex={a.depth === 1 ? 0 : -1}
              aria-label={describe(a)}
              onMouseEnter={() => setHover(a.id)}
              onFocus={() => setHover(a.id)}
              onBlur={() => setHover(null)}
              onClick={() => activate(a)}
              onKeyDown={(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); activate(a) } }}
            >
              {a.bands.map((band, i) => (
                <path
                  key={band.part}
                  d={arcPath(band.start, band.end, inner, out, SEGMENT_GAP, [i === 0, i === a.bands.length - 1])}
                  data-part={band.part}
                  style={{ fill: fillOf(band.part, a.depth) }}
                />
              ))}
            </g>
          )
        })}
        <circle
          className="barca-healthmap-hole"
          r={hole}
          role={parent !== undefined ? 'button' : undefined}
          tabIndex={parent !== undefined ? 0 : undefined}
          aria-label={parent !== undefined ? `Up to ${parent === ROOT ? 'All pipelines' : tree.entries.get(parent)?.label ?? 'All pipelines'}` : undefined}
          onClick={() => parent !== undefined && onFocus(parent)}
          onKeyDown={(e) => { if (parent !== undefined && (e.key === 'Enter' || e.key === ' ')) { e.preventDefault(); onFocus(parent) } }}
          onMouseEnter={() => setHover(null)}
        >
          {parent !== undefined && <title>Up one level</title>}
        </circle>
        {/* The scope in the terms of the gap, in real type: its causes as served, in nodes. */}
        <foreignObject x={-hole} y={-hole} width={hole * 2} height={hole * 2} pointerEvents="none">
          <div className="barca-healthmap-center" data-fresh={rows.length === 0 || undefined}>
            {rows.length === 0 ? (
              <>
                <strong>{center.nodes.toLocaleString()}</strong>
                <span>{center.nodes === 1 ? 'node, fresh' : 'nodes, all fresh'}</span>
              </>
            ) : (
              <>
                {rows.slice(0, CENTER_ROWS).map((row) => (
                  <span key={row.cause} className="barca-healthmap-center-row" data-alarm={row.alarm || undefined}>
                    <strong style={{ color: mapPartColor(row.cause) }}>{row.count.toLocaleString()}</strong> {row.label}
                  </span>
                ))}
                <span className="barca-healthmap-center-unit">nodes{rows.length > CENTER_ROWS ? ', and more' : ''}</span>
              </>
            )}
          </div>
        </foreignObject>
      </svg>
      <figcaption className="barca-healthmap-caption" aria-live="polite">
        <span className="barca-healthmap-where">{where ?? (hovered ? '' : parent !== undefined ? 'click the centre to go up' : '')}</span>
        <span className="barca-healthmap-name">{name}</span>
        <span className="barca-healthmap-summary">{summary}</span>
        <span className="barca-healthmap-estimate">{estimate}</span>
      </figcaption>
      <ul className="barca-healthmap-key" aria-label="Colours, by why a node is not fresh">
        {MAP_PARTS.map((part) => (
          <li key={part} title={mapPartMeaning(part)}><i style={{ background: mapPartColor(part) }} />{mapPartLabel(part)}</li>
        ))}
      </ul>
    </figure>
  )
}
