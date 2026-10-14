import { Handle, Position, type NodeProps } from '@xyflow/react'
import { FolderOpen, ArrowUpRight } from 'lucide-react'
import { StatusDot } from '@/components'
import { statusMeta } from '@/lib/status'
import type { GraphNode } from '@/lib/graph'

export function GroupNode({ data, selected }: NodeProps<GraphNode>) {
  return (
    <div
      className="barca-group-node"
      data-status={data.status}
      data-kind="group"
      data-selected={selected}
      style={{ borderLeftColor: statusMeta(data.status).color }}
      title={data.stateHint}
    >
      <Handle
        type="target"
        position={data.direction === 'TB' ? Position.Top : Position.Left}
      />
      <div className="barca-group-node-title">
        <FolderOpen size={16} />
        <strong>{data.name}</strong>
        <ArrowUpRight size={13} />
      </div>
      <div className="barca-group-node-summary">{data.summary}</div>
      <div className="barca-group-node-output">↳ {data.outputName}</div>
      <div className="barca-group-node-health">
        <StatusDot status={data.status} size={6} />
        {data.metric}
      </div>
      <Handle
        type="source"
        position={data.direction === 'TB' ? Position.Bottom : Position.Right}
      />
    </div>
  )
}
