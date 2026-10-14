export interface ArtifactSchema {
  type: string | null
  rows: number | null
  columns: { name: string; type: string | null }[]
  fields: boolean
  itemTypes: string[]
  note: string | null
}
/** Validate the inspector's open JSON shape without inventing column types. */
export function artifactSchema(value: unknown): ArtifactSchema {
  const empty: ArtifactSchema = { type: null, rows: null, columns: [], fields: false, itemTypes: [], note: null }
  if (!value || typeof value !== 'object' || Array.isArray(value)) return empty
  const shape = value as Record<string, unknown>
  const columns: ArtifactSchema['columns'] = []
  if (Array.isArray(shape.columns)) {
    for (const column of shape.columns) {
      if (!column || typeof column !== 'object' || typeof column.name !== 'string') continue
      columns.push({ name: column.name, type: typeof column.type === 'string' ? column.type : null })
    }
  } else if (Array.isArray(shape.keys)) {
    for (const key of shape.keys) if (typeof key === 'string') columns.push({ name: key, type: null })
  }
  return {
    type: typeof shape.type === 'string' ? shape.type : null,
    rows: typeof shape.rows === 'number' && Number.isFinite(shape.rows) ? shape.rows : null,
    columns,
    fields: shape.type === 'dict' || (!Array.isArray(shape.columns) && Array.isArray(shape.keys)),
    itemTypes: Array.isArray(shape.item_types) ? shape.item_types.filter((t): t is string => typeof t === 'string') : [],
    note: typeof shape.note === 'string' ? shape.note : null,
  }
}
