import { artifactSchema } from '@/lib/artifactSchema'
import type { NodeStatus } from '@/lib/types'

export function ArtifactSchema({ node }: { node: NodeStatus }) {
  const schema = artifactSchema(node.shape)
  const last = node.last_materialization
  if (!last) return <p className="barca-schema-empty">Not materialized yet. Run this asset to inspect its output.</p>
  if (last.status !== 'success') return <p className="barca-schema-empty">The latest attempt failed. No schema is available for that attempt.</p>
  if (!last.artifact) return <p className="barca-schema-empty">No output artifact was recorded.</p>
  return <>
    <div className="barca-schema-meta">
      {schema.type && <strong>{schema.type}</strong>}
      {schema.rows !== null && <span>{schema.rows.toLocaleString()} {schema.type === 'list' && !schema.columns.length ? 'items' : 'rows'}</span>}
      {schema.itemTypes.length > 0 && !schema.columns.length && <span>items: {schema.itemTypes.join(' | ')}</span>}
      {schema.columns.length > 0 && <span>{schema.columns.length} {schema.fields ? 'fields' : 'columns'}</span>}
      {last.format && <span>{last.format}</span>}
    </div>
    <p className="barca-schema-provenance">Materialized {last.created_at} UTC{last.partition ? ` · key ${last.partition}` : ''}</p>
    {node.partitioned && <p className="barca-schema-empty">Schema of this key only; other keys may differ.</p>}
    {last.format === 'pickle' && <p className="barca-schema-empty">Top-level type or constructor only. Pickle fields are unavailable without loading the object.</p>}
    {schema.note && <p className="barca-schema-empty">{schema.note}</p>}
    {schema.columns.length > 0 ? <div className="barca-schema-scroll">
      <table className="barca-schema-table" aria-label={`${node.name} output schema`}>
        <thead><tr><th>{schema.fields ? 'Field' : 'Column'}</th><th>Type</th></tr></thead>
        <tbody>{schema.columns.map((c, i) => <tr key={`${c.name}-${i}`}><td>{c.name}</td><td>{c.type ?? 'not recorded'}</td></tr>)}</tbody>
      </table>
    </div> : !schema.note && <p className="barca-schema-empty">{schema.type ? 'This output has no recorded columns.' : 'Schema information is unavailable for this artifact.'}</p>}
  </>
}
