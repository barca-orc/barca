import { BaseEdge, getBezierPath, type Edge, type EdgeProps } from '@xyflow/react'
import { routeMiddle, routePath } from '@/lib/graphLayout'
import type { GraphEdgeData } from '@/lib/graph'

/**
 * An edge along its route from the layout (`routeEdges`): straight through the ranks it passes,
 * between their boxes, and curving only in the gaps between ranks, so it never crosses a card.
 * Its ends follow the handles React Flow measured. A loose edge (no route) is a plain curve.
 */
export function RoutedEdge({ sourceX, sourceY, targetX, targetY, sourcePosition, targetPosition, data, label, style, markerEnd, interactionWidth }: EdgeProps<Edge<GraphEdgeData>>) {
  const route = data?.route
  if (!route || route.length < 2) {
    const [path, labelX, labelY] = getBezierPath({ sourceX, sourceY, targetX, targetY, sourcePosition, targetPosition })
    return <BaseEdge path={path} label={label} labelX={labelX} labelY={labelY} style={style} markerEnd={markerEnd} interactionWidth={interactionWidth} labelBgPadding={[4, 2]} labelBgBorderRadius={3} />
  }
  const points = [{ x: sourceX, y: sourceY }, ...route.slice(1, -1), { x: targetX, y: targetY }]
  const middle = routeMiddle(points)
  return (
    <BaseEdge
      path={routePath(points, data.direction)}
      label={label}
      labelX={middle.x}
      labelY={middle.y}
      style={style}
      markerEnd={markerEnd}
      interactionWidth={interactionWidth}
      labelBgPadding={[4, 2]}
      labelBgBorderRadius={3}
    />
  )
}
