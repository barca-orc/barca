import { Handle, Position, type NodeProps } from '@xyflow/react'
import { DagNode, DAG_NODE_WIDTH } from '@/components'
import { nodeKindLabel } from '@/lib/status'
import type { GraphNode } from '@/lib/graph'

/**
 * Custom React Flow node: the design-system DagNode plus the source/target
 * handles edges attach to. The handles are visually hidden (DagNode draws its
 * own edge dots) — see barca-flow.css.
 */
export function AssetNode({ id, data, selected }: NodeProps<GraphNode>) {
  const { kind } = data
  const leaf = kind !== 'group' && kind !== 'port' ? kind : null
  return (
    <div style={{ width: DAG_NODE_WIDTH }}>
      <Handle type="target" position={data.direction === "TB" ? Position.Top : Position.Left} />
      <DagNode
        name={data.name}
        kind={data.kind}
        kindLabel={leaf && leaf !== 'asset' ? nodeKindLabel({ id, kind: leaf }) : undefined}
        tone={data.tone}
        glyph={data.glyph}
        label={data.label}
        lastFailed={data.lastFailed}
        blockedBy={data.blockedBy}
        pinned={data.pinned}
        live={data.live}
        stateHint={data.stateHint}
        selected={selected}
      />
      <Handle type="source" position={data.direction === "TB" ? Position.Bottom : Position.Right} />
    </div>
  )
}
