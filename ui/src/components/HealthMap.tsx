import { useId, useMemo, useState } from 'react'
import { arcPath, healthSummary, sunburst, treePath, ROOT, type Arc, type HealthTree } from '@/lib/healthTree'
import { healthDisplay, type Health } from '@/lib/nodeHealth'

export interface HealthMapProps {
  tree: HealthTree
  /** The scope at the centre: the root, a group or a file. */
  focus: string
  /** Zoom to a scope (a segment with members, or the centre to go up). */
  onFocus: (id: string) => void
  /** Open a node (a leaf segment). */
  onOpen: (id: string) => void
  /** Side of the square, in px (the drawing scales with it). */
  size?: number
}

const RINGS = 2

/** The map's four colours: what a health is drawn as. */
type Family = 'failing' | 'attention' | 'ok' | 'unknown'

const FAMILY: Record<Health, Family> = {
  failing: 'failing',
  blocked: 'attention',
  suspect: 'attention',
  behind: 'attention',
  waiting: 'unknown',
  never_run: 'unknown',
  ok: 'ok',
}

/** What the map's colours mean: failing, needs attention, ok, not judged yet. */
const KEY: readonly (readonly [Family, string])[] = [
  ['failing', 'failing'],
  ['attention', 'needs attention'],
  ['ok', 'ok'],
  ['unknown', 'not known yet'],
]

/** A family's inner and rim tones (map tokens, so both themes follow). */
const TONES: Record<Family, readonly [string, string]> = {
  failing: ['var(--map-failing-0)', 'var(--map-failing-1)'],
  attention: ['var(--map-attention-0)', 'var(--map-attention-1)'],
  ok: ['var(--map-ok-0)', 'var(--map-ok-1)'],
  unknown: ['var(--map-unknown-0)', 'var(--map-unknown-1)'],
}

/** Geometry, as fractions of the radius: the hole, then the rings, parted by gaps. */
const HOLE = 0.42
const RING_GAP = 0.008
/** The gap between neighbouring segments, px at the drawing's own size. */
const SEGMENT_GAP = 0.6

/** The arc ids lit while `hover` is under the pointer: it, what is inside it, and what holds it. */
function litIds(arcs: readonly Arc[], hover: string | null): Set<string> | null {
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

/**
 * barca · HealthMap
 * The scope as a sunburst: the centre is the scope, each ring one level down (groups are the
 * unit). A segment's angle is how much is wrong underneath (unhealthy keys or nodes, with a thin
 * floor so a healthy group stays visible); its colour is the worst health inside, in three: red
 * failing, yellow needs attention, green ok (grey: not known yet). Members too thin to draw are
 * one quiet `N smaller` segment. The centre names the scope, or the segment under the pointer,
 * whose branch is lit while the rest dims. Click a segment to make it the centre, a node to open
 * it, the centre to go up.
 */
export function HealthMap({ tree, focus, onFocus, onOpen, size = 300 }: HealthMapProps) {
  const arcs = useMemo(() => sunburst(tree, focus, RINGS), [tree, focus])
  const [hover, setHover] = useState<string | null>(null)
  const uid = useId().replace(/:/g, '')
  const lit = useMemo(() => litIds(arcs, hover), [arcs, hover])
  const center = tree.nodes.get(focus)
  if (!center) return null

  const half = size / 2
  const hole = half * HOLE
  const ring = (half - hole - half * RING_GAP * RINGS) / RINGS
  const radii = (a: Arc) => {
    const inner = hole + half * RING_GAP * a.depth + ring * (a.depth - 1)
    return [inner, inner + ring] as const
  }
  const parent = tree.parent.get(focus)
  const hovered = hover ? arcs.find((a) => a.id === hover) ?? null : null
  const hoveredNode = hovered && hovered.kind !== 'rest' ? tree.nodes.get(hovered.id) : null
  const activate = (a: Arc) => (a.kind === 'node' ? onOpen(a.id) : a.kind === 'rest' ? onFocus(a.parent) : onFocus(a.id))

  // What the centre says: the segment under the pointer, else the scope.
  const title = hovered ? hovered.label : focus === ROOT ? 'All pipelines' : center.label
  const where = hovered && hovered.kind !== 'rest' ? treePath(tree, hovered.id).slice(1, -1).map((n) => n.label).join(' › ') : null
  const detail = hoveredNode
    ? healthSummary(hoveredNode, 2) || healthDisplay(hoveredNode.health).label
    : hovered
      ? healthDisplay(hovered.health).label
      : healthSummary(center, 2) || 'Nothing needs attention'
  const count = (hovered ?? center).leaves

  return (
    <figure className="barca-healthmap" aria-label={`Health map of ${center.label}`}>
      <svg
        width={size}
        height={size}
        viewBox={`${-half} ${-half} ${size} ${size}`}
        role="group"
        data-hovering={hover !== null || undefined}
        onMouseLeave={() => setHover(null)}
      >
        <defs>
          {(Object.keys(TONES) as Family[]).flatMap((f) => [
            // Each family brightens toward the rim, so the rings read as one lit disk.
            <radialGradient key={f} id={`${uid}-${f}`} gradientUnits="userSpaceOnUse" cx="0" cy="0" r={half}>
              <stop offset={HOLE} style={{ stopColor: TONES[f][0] }} />
              <stop offset="1" style={{ stopColor: TONES[f][1] }} />
            </radialGradient>,
            // The `N smaller` segment: slate, faintly tinted by its family, so it reads as the rest.
            <radialGradient key={`${f}-rest`} id={`${uid}-${f}-rest`} gradientUnits="userSpaceOnUse" cx="0" cy="0" r={half}>
              <stop offset={HOLE} style={{ stopColor: `color-mix(in oklch, ${TONES[f][0]} 18%, var(--map-unknown-0))` }} />
              <stop offset="1" style={{ stopColor: `color-mix(in oklch, ${TONES[f][1]} 18%, var(--map-unknown-1))` }} />
            </radialGradient>,
          ])}
        </defs>
        {arcs.map((a) => {
          const [inner, outer] = radii(a)
          const family = FAMILY[a.health]
          return (
            <path
              key={a.id}
              className="barca-healthmap-arc"
              d={arcPath(a.start, a.end, inner, outer, SEGMENT_GAP)}
              fill={`url(#${uid}-${family}${a.kind === 'rest' ? '-rest' : ''})`}
              data-family={family}
              data-kind={a.kind}
              data-depth={a.depth}
              data-lit={lit === null ? undefined : lit.has(a.id)}
              role="button"
              tabIndex={a.depth === 1 ? 0 : -1}
              aria-label={`${a.label}: ${healthDisplay(a.health).label}, ${a.leaves.toLocaleString()} ${a.leaves === 1 ? 'node' : 'nodes'}`}
              onMouseEnter={() => setHover(a.id)}
              onFocus={() => setHover(a.id)}
              onBlur={() => setHover(null)}
              onClick={() => activate(a)}
              onKeyDown={(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); activate(a) } }}
            />
          )
        })}
        <circle
          className="barca-healthmap-hole"
          r={hole - half * RING_GAP}
          role={parent !== undefined ? 'button' : undefined}
          tabIndex={parent !== undefined ? 0 : undefined}
          aria-label={parent !== undefined ? `Up to ${tree.nodes.get(parent)?.label ?? 'All pipelines'}` : undefined}
          onClick={() => parent !== undefined && onFocus(parent)}
          onKeyDown={(e) => { if (parent !== undefined && (e.key === 'Enter' || e.key === ' ')) { e.preventDefault(); onFocus(parent) } }}
          onMouseEnter={() => setHover(null)}
        />
        {/* The centre's words are HTML: real type, wrapped and cut cleanly, at any scale. */}
        <foreignObject x={-hole * 0.86} y={-hole * 0.66} width={hole * 1.72} height={hole * 1.32} pointerEvents="none">
          <div className="barca-healthmap-center">
            {where && <span className="barca-healthmap-where">{where}</span>}
            <strong className="barca-healthmap-title">{title}</strong>
            <span className="barca-healthmap-detail" data-family={FAMILY[(hoveredNode ?? hovered ?? center).health]}>
              {/* A part never breaks inside itself (`1 blocked`); lines break between parts. */}
              {detail.split(' · ').map((part, i) => (
                <span key={i}>{i > 0 && ' · '}<span className="barca-healthmap-part">{part}</span></span>
              ))}
            </span>
            <span className="barca-healthmap-count">
              {count.toLocaleString()} {count === 1 ? 'node' : 'nodes'}
              {!hovered && parent !== undefined && ' · click to go up'}
            </span>
          </div>
        </foreignObject>
      </svg>
      <ul className="barca-healthmap-key" aria-label="Colours">
        {KEY.map(([family, word]) => (
          <li key={family}><i data-family={family} />{word}</li>
        ))}
      </ul>
    </figure>
  )
}
