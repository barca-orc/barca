import { useMemo, useState } from 'react'
import { arcPath, healthSummary, mapUnit, ringRadii, sunburst, treePath, worstOf, ROOT, type Arc, type RollupEntry, type RollupTree, type Unit } from '@/lib/rollupView'
import { HEALTH_ORDER, healthDisplay, healthFamily, isUnhealthy, type HealthFamily } from '@/lib/healthDisplay'
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
  /** Side of the square, in px (the drawing scales with it). */
  size?: number
}

/** How many levels below the centre are drawn: a branch ends where its members do. */
const MAX_RINGS = 6

/** What the map's colours mean: failing, needs attention, ok, not judged yet. */
const KEY: readonly (readonly [HealthFamily, string])[] = [
  ['failing', 'failing'],
  ['attention', 'needs attention'],
  ['ok', 'ok'],
  ['unknown', 'not known yet'],
]

/**
 * Geometry, as fractions of the radius. A small hole for one number; rings that thin out by
 * `DECAY` each level, so the deep levels read as fine texture and the near ones as the shape;
 * hairline gaps (px, at the drawing's size) between rings and between neighbours.
 */
const HOLE = 0.21
const RING_SHAPE = { firstMax: 0.27, decay: 0.8, gap: 0.8 }
const SEGMENT_GAP = 0.8

/**
 * A segment's colour: its family, a step lighter and a touch softer at each level out (same hue,
 * so a family stays itself), the way a disk map's deep folders read paler than their parents.
 */
function fillOf(a: Arc): string {
  const family = healthFamily(a.health)
  const k = a.depth - 1
  return k === 0 ? `var(--map-${family})` : `oklch(from var(--map-${family}) calc(l + ${(0.025 * k).toFixed(3)}) calc(c * ${(1 - 0.05 * k).toFixed(2)}) h)`
}

/** `12 assets`; `3 nodes` in a scope with no assets, where the map is sized by nodes. */
function sizeWords(n: number, unit: Unit): string {
  return plural(n, unit === 'assets' ? 'asset' : 'node')
}

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
 * The centre's one number, in the map's unit (assets, or nodes in a scope with no assets): how
 * many are in the scope's worst health, or, when all is well, how many there are. The unit is
 * said under it, so it is never read against the bar's nodes.
 */
function headline(entry: RollupEntry, unit: Unit): { value: number; word: string; family: HealthFamily } {
  const counts = unit === 'assets' ? entry.asset_health : entry.health
  const one = unit === 'assets' ? 'asset' : 'node'
  const worst = HEALTH_ORDER.find((h) => isUnhealthy(h) && counts[h] > 0)
  if (worst) return { value: counts[worst], word: `${healthDisplay(worst).label} ${counts[worst] === 1 ? one : `${one}s`}`, family: healthFamily(worst) }
  const all = unit === 'assets' ? entry.assets : entry.nodes
  return { value: all, word: `${all === 1 ? one : `${one}s`}, all ok`, family: 'ok' }
}

/**
 * barca · HealthMap
 * The scope as a sunburst, drawn the way a disk map draws a folder: the centre is the scope, each
 * ring one level down (groups, then their groups, then assets, then a partitioned asset's keys),
 * thinner as it goes out, and a branch ends where its members do. A segment's angle is how many
 * assets are underneath; its colour is the worst health inside: red failing, amber needs
 * attention, green ok, slate not known yet, paler at each level out. Members too thin to draw are
 * one `N smaller` segment per colour; a partitioned asset with room shows its keys outside it
 * (failed, not up to date, ok). The centre holds one number, with its unit (how many assets are
 * in the scope's worst health); the line under the map names the segment under the pointer, whose branch is lit
 * while the rest dims. Click a segment to make it the centre, an asset or its keys to open it,
 * the centre to go up.
 */
export function HealthMap({ tree, focus, onFocus, onOpen, size = 300 }: HealthMapProps) {
  const arcs = useMemo(() => sunburst(tree, focus, MAX_RINGS, 0.008), [tree, focus])
  const [hover, setHover] = useState<string | null>(null)
  const lit = useMemo(() => litIds(arcs, hover), [arcs, hover])
  const center = tree.entries.get(focus)
  if (!center) return null
  const unit = mapUnit(center)

  const half = size / 2
  const outer = half - 1
  const hole = outer * HOLE
  const rings = ringRadii(arcs.reduce((max, a) => Math.max(max, a.depth), 0), hole, outer, RING_SHAPE)
  const parent = center.parent ?? undefined
  const hovered = hover ? arcs.find((a) => a.id === hover) ?? null : null
  const hoveredEntry = hovered && hovered.kind !== 'rest' && hovered.kind !== 'keys' ? tree.entries.get(hovered.id) : null
  const activate = (a: Arc) => (a.kind === 'node' ? onOpen(a.id) : a.kind === 'keys' ? onOpen(a.parent) : a.kind === 'rest' ? onFocus(a.parent) : onFocus(a.id))
  const top = headline(center, unit)

  // The line under the map: the segment under the pointer, else the scope.
  const name = hovered ? hovered.label : focus === ROOT ? 'All pipelines' : center.label
  // Where the segment sits: the path to it, or to what holds it (`N smaller`, a node's keys).
  const synthetic = hovered?.kind === 'rest' || hovered?.kind === 'keys'
  const where = hovered ? treePath(tree, synthetic ? hovered.parent : hovered.id).slice(1, synthetic ? undefined : -1).map((n) => n.label).join(' › ') : null
  // The words count what the map is sized in, so the line and the segment agree.
  const countsOf = (e: RollupEntry) => (unit === 'assets' ? e.asset_health : e.health)
  const summary = hoveredEntry
    ? healthSummary(countsOf(hoveredEntry), 2) || healthDisplay(worstOf(hoveredEntry.health)).label
    : hovered
      ? healthDisplay(hovered.health).label
      : healthSummary(countsOf(center), 3) || 'nothing needs attention'
  const amount = hovered?.keys
    ? `${hovered.keys.count.toLocaleString()} of ${hovered.keys.of.toLocaleString()} keys`
    : sizeWords(hovered ? hovered.size : unit === 'assets' ? center.assets : center.nodes, unit)

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
        {arcs.map((a) => {
          const [inner, out] = rings[a.depth - 1]!
          return (
            <path
              key={a.id}
              className="barca-healthmap-arc"
              d={arcPath(a.start, a.end, inner, out, SEGMENT_GAP)}
              style={{ fill: fillOf(a) }}
              data-kind={a.kind}
              data-lit={lit === null ? undefined : lit.has(a.id)}
              role="button"
              tabIndex={a.depth === 1 ? 0 : -1}
              aria-label={`${a.label}: ${healthDisplay(a.health).label}, ${a.keys ? `${a.keys.count.toLocaleString()} of ${a.keys.of.toLocaleString()} keys` : sizeWords(a.size, unit)}`}
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
          r={hole}
          data-family={top.family}
          role={parent !== undefined ? 'button' : undefined}
          tabIndex={parent !== undefined ? 0 : undefined}
          aria-label={parent !== undefined ? `Up to ${parent === ROOT ? 'All pipelines' : tree.entries.get(parent)?.label ?? 'All pipelines'}` : undefined}
          onClick={() => parent !== undefined && onFocus(parent)}
          onKeyDown={(e) => { if (parent !== undefined && (e.key === 'Enter' || e.key === ' ')) { e.preventDefault(); onFocus(parent) } }}
          onMouseEnter={() => setHover(null)}
        >
          {parent !== undefined && <title>Up one level</title>}
        </circle>
        {/* One number, in real type, with its unit: how many are in the scope's worst health. */}
        <foreignObject x={-hole} y={-hole} width={hole * 2} height={hole * 2} pointerEvents="none">
          <div className="barca-healthmap-center" data-family={top.family}>
            <strong>{top.value.toLocaleString()}</strong>
            <span>{top.word}</span>
          </div>
        </foreignObject>
      </svg>
      <figcaption className="barca-healthmap-caption" aria-live="polite">
        <span className="barca-healthmap-where">{where ?? (hovered ? '' : parent !== undefined ? 'click the centre to go up' : '')}</span>
        <span className="barca-healthmap-name">{name}</span>
        <span className="barca-healthmap-summary" data-family={healthFamily(hoveredEntry ? worstOf(hoveredEntry.health) : hovered ? hovered.health : worstOf(center.health))}>
          {summary} · {amount}
        </span>
      </figcaption>
      <ul className="barca-healthmap-key" aria-label="Colours">
        {KEY.map(([family, word]) => (
          <li key={family}><i data-family={family} />{word}</li>
        ))}
      </ul>
    </figure>
  )
}
