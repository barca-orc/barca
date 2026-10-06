import { Handle, Position, type NodeProps } from '@xyflow/react'
import { Database, Radio, Zap } from 'lucide-react'
import { match } from 'ts-pattern'
import { DagNode, DAG_NODE_WIDTH } from '@/components'
import type { NodeKind } from '@/lib/types'
import type { GraphNode } from '@/lib/graph'

function kindIcon(kind: NodeKind) {
  return match(kind)
    .with('asset', () => <Database size={12} />)
    .with('sensor', () => <Radio size={12} />)
    .with('task', () => <Zap size={12} />)
    .exhaustive()
}

/**
 * Custom React Flow node: the design-system DagNode plus the source/target
 * handles edges attach to. The handles are visually hidden (DagNode draws its
 * own edge dots) — see barca-flow.css.
 */
export function AssetNode({ data, selected }: NodeProps<GraphNode>) {
  return (
    <div style={{ width: DAG_NODE_WIDTH }}>
      <Handle type="target" position={Position.Left} />
      <DagNode
        name={data.name}
        kind={data.kind}
        status={data.status}
        metric={data.metric}
        selected={selected}
        icon={kindIcon(data.kind)}
      />
      <Handle type="source" position={Position.Right} />
    </div>
  )
}
