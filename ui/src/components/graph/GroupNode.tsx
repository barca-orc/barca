import { Handle, Position, type NodeProps } from '@xyflow/react'
import { FolderOpen, ArrowUpRight } from 'lucide-react'
import { StatusDot } from '@/components'
import type { GraphNode } from '@/lib/graph'

/** A group of nodes in the graph: its name, what it holds, and its most urgent fact in words. */
export function GroupNode({ data, selected }: NodeProps<GraphNode>) {
  return (
    <div
      className="barca-group-node"
      data-tone={data.tone}
      data-kind="group"
      data-selected={selected}
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
      {data.outputName && <div className="barca-group-node-output">↳ {data.outputName}</div>}
      <div className="barca-group-node-health">
        <StatusDot tone={data.tone} live={data.live} size={6} />
        {data.label}
      </div>
      <Handle
        type="source"
        position={data.direction === 'TB' ? Position.Bottom : Position.Right}
      />
    </div>
  )
}
