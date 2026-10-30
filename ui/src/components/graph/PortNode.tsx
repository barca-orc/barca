import { Handle, Position, type NodeProps } from '@xyflow/react'
import { MiddleName } from '@/components'
import type { GraphNode } from '@/lib/graph'

/** An input from outside the view: where an edge comes from, faded, and nothing to open. */
export function PortNode({ id, data }: NodeProps<GraphNode>) {
  return (
    <div className="barca-port-node" data-kind="port">
      <MiddleName name={data.name} title={data.summary ?? `External input: ${id}`} />
      <Handle type="source" position={data.direction === 'TB' ? Position.Bottom : Position.Right} />
    </div>
  )
}
