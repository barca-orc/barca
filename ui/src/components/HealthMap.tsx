import { useMemo, useState } from 'react'
import { arcCentroid, arcPath, healthSummary, sunburst, treePath, ROOT, type Arc, type HealthTree } from '@/lib/healthTree'
import { healthDisplay } from '@/lib/nodeHealth'

export interface HealthMapProps {
  tree: HealthTree
  /** The scope at the centre: the root, a group or a file. */
  focus: string
  /** Zoom to a scope (a segment with members, or the centre to go up). */
  onFocus: (id: string) => void
  /** Open a node (a leaf segment). */
  onOpen: (id: string) => void
  /** Side of the square, in px. */
  size?: number
}

const RINGS = 3

function shorten(label: string, max: number): string {
  return label.length <= max ? label : `${label.slice(0, max - 1)}…`
}

/**
 * barca · HealthMap
 * The scope as a sunburst: the centre is the scope, each ring one level down. A segment's angle
 * is how much is wrong underneath (unhealthy keys or nodes, with a thin floor so a healthy group
 * stays visible); its colour is the worst health inside. Click a segment to make it the centre,
 * a node to open it, the centre to go up. The line under the map names what the pointer is on.
 */
export function HealthMap({ tree, focus, onFocus, onOpen, size = 300 }: HealthMapProps) {
  const arcs = useMemo(() => sunburst(tree, focus, RINGS), [tree, focus])
  const [hover, setHover] = useState<string | null>(null)
  const center = tree.nodes.get(focus)
  if (!center) return null
  const half = size / 2
  const r0 = size * 0.17
  const ring = (half - r0 - 2) / RINGS
  const radii = (a: Arc) => [r0 + (a.depth - 1) * ring + 1, r0 + a.depth * ring] as const
  const shown = hover ? tree.nodes.get(hover) : center
  const parent = tree.parent.get(focus)
  const path = hover ? treePath(tree, hover).slice(1).map((n) => n.label).join(' › ') : null
  const activate = (a: Arc) => (a.kind === 'node' ? onOpen(a.id) : onFocus(a.id))
  const summary = healthSummary(center)

  return (
    <figure className="barca-healthmap" aria-label={`Health map of ${center.label}`}>
      <svg width={size} height={size} viewBox={`${-half} ${-half} ${size} ${size}`} role="group" onMouseLeave={() => setHover(null)}>
        <circle
          className="barca-healthmap-center"
          r={r0 - 2}
          data-health={center.health}
          role={parent !== undefined ? 'button' : undefined}
          tabIndex={parent !== undefined ? 0 : undefined}
          aria-label={parent !== undefined ? `Up to ${tree.nodes.get(parent)?.label ?? 'All pipelines'}` : undefined}
          onClick={() => parent !== undefined && onFocus(parent)}
          onKeyDown={(e) => { if (parent !== undefined && (e.key === 'Enter' || e.key === ' ')) { e.preventDefault(); onFocus(parent) } }}
          onMouseEnter={() => setHover(null)}
        />
        <text className="barca-healthmap-center-label" y={-4}>{shorten(focus === ROOT ? 'All' : center.label, 14)}</text>
        <text className="barca-healthmap-center-sub" y={12}>{center.leaves.toLocaleString()} nodes</text>
        {arcs.map((a) => {
          const [inner, outer] = radii(a)
          return (
            <path
              key={a.id}
              className="barca-healthmap-arc"
              d={arcPath(a.start, a.end, inner, outer)}
              data-health={a.health}
              data-depth={a.depth}
              data-hover={hover === a.id || undefined}
              role="button"
              tabIndex={a.depth === 1 ? 0 : -1}
              aria-label={`${a.label}: ${healthDisplay(a.health).label}`}
              onMouseEnter={() => setHover(a.id)}
              onFocus={() => setHover(a.id)}
              onClick={() => activate(a)}
              onKeyDown={(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); activate(a) } }}
            />
          )
        })}
        {arcs.filter((a) => a.depth === 1 && a.end - a.start > 0.1).map((a) => {
          const [inner, outer] = radii(a)
          const c = arcCentroid(a.start, a.end, inner, outer)
          return <text key={`l:${a.id}`} className="barca-healthmap-arc-label" x={c.x} y={c.y + 4}>{shorten(a.label, 12)}</text>
        })}
      </svg>
      <figcaption className="barca-healthmap-caption">
        {shown && hover ? (
          <>
            <span className="barca-healthmap-path">{path}</span>
            <span>{healthSummary(shown) || healthDisplay(shown.health).label} · {shown.leaves.toLocaleString()} {shown.leaves === 1 ? 'node' : 'nodes'}</span>
          </>
        ) : (
          <span>{summary || 'Nothing needs attention'}</span>
        )}
      </figcaption>
    </figure>
  )
}
