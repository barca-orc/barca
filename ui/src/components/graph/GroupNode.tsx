import { Handle, Position, type NodeProps } from '@xyflow/react'
import { FolderOpen, ArrowUpRight } from 'lucide-react'
import { StatusGlyph } from '@/components'
import { CauseDot } from '@/components/CauseDot'
import { toneGlyph } from '@/lib/glyph'
import type { GraphNode } from '@/lib/graph'

/** A part's mark: a dot in the health map's colour for a cause or fresh, the glyph for run work. */
function PartMark({ part }: { part: NonNullable<GraphNode['data']['parts']>[number] }) {
  return part.part ? <CauseDot part={part.part} /> : <StatusGlyph glyph={part.glyph} live={part.glyph === 'running'} size={12} />
}

/**
 * A group of nodes in the graph: its name, what it holds, and where its nodes stand, in the
 * health map's words and colours: the leading part first in words (`2 failing`), then the rest
 * with a dot and the full count and cause: a failure is never hidden behind run work. The tooltip names
 * every part.
 */
export function GroupNode({ data, selected }: NodeProps<GraphNode>) {
  const glyph = data.glyph ?? toneGlyph(data.tone)
  const [lead, ...rest] = data.parts ?? []
  return (
    <div
      className="barca-group-node"
      data-tone={data.tone}
      data-glyph={glyph}
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
        {lead ? (
          <>
            <span className="barca-group-node-lead">
              <PartMark part={lead} />
              {lead.text}
            </span>
            {rest.map((part) => (
              <span key={part.glyph + part.word} className="barca-group-node-part" title={part.text} aria-label={part.text}>
                <PartMark part={part} />
                {part.text}
              </span>
            ))}
          </>
        ) : (
          <>
            <StatusGlyph glyph={glyph} live={data.live} size={12} />
            {data.label}
          </>
        )}
      </div>
      <Handle
        type="source"
        position={data.direction === 'TB' ? Position.Bottom : Position.Right}
      />
    </div>
  )
}
