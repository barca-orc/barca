import { Handle, Position, type NodeProps } from '@xyflow/react'
import { Layers } from 'lucide-react'
import { StatusDot, DAG_NODE_WIDTH } from '@/components'
import type { GraphNode } from '@/lib/graph'
import { statusMeta } from '@/lib/status'

/** A summary represents real steps; clicking expands it, never triggers a run. */
export function ChainNode({ data }: NodeProps<GraphNode>) {
  const meta = statusMeta(data.status)
  return <div className="barca-chain-node" style={{ width: DAG_NODE_WIDTH, borderColor: meta.line }} title={data.stateHint}>
    <Handle type="target" position={data.direction === 'TB' ? Position.Top : Position.Left} />
    <div className="barca-chain-title"><StatusDot status={data.status} /><strong>{data.name}</strong><Layers size={12} /></div>
    <div className="barca-chain-counts">{data.metric}</div>
    <button className="barca-chain-expand" onClick={event => { event.stopPropagation(); data.onExpand?.() }}>Expand steps</button>
    <Handle type="source" position={data.direction === 'TB' ? Position.Bottom : Position.Right} />
  </div>
}
