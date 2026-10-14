import { describe, expect, it } from 'vitest'
import { artifactSchema } from './artifactSchema'

describe('artifactSchema', () => {
  it('preserves table columns, nullable types, and zero rows', () => {
    expect(artifactSchema({ type: 'table', rows: 0, columns: [{ name: 'value', type: 'int64 | null' }] })).toMatchObject({
      type: 'table', rows: 0, columns: [{ name: 'value', type: 'int64 | null' }], fields: false,
    })
  })
  it('does not infer dictionary field types from names', () => {
    expect(artifactSchema({ type: 'dict', keys: ['revenue'] })).toMatchObject({ columns: [{ name: 'revenue', type: null }], fields: true })
  })
  it('keeps inspector failure notes and handles unsupported shapes', () => {
    expect(artifactSchema({ note: 'artifact file not found' }).note).toBe('artifact file not found')
    expect(artifactSchema(null).columns).toEqual([])
    expect(artifactSchema({ columns: [null, {}, { name: 'x' }] }).columns).toEqual([{ name: 'x', type: null }])
  })
})


describe('schema type families', () => {
  it.each(['null', 'bool', 'int', 'float', 'str', 'list', 'dict', 'set', 'tuple', 'bytes', 'custom.Thing'])(
    'preserves top-level %s', type => expect(artifactSchema({ type }).type).toBe(type),
  )
  it.each(['bool', 'int8', 'int16', 'int32', 'int64', 'uint8', 'uint64', 'halffloat', 'float', 'double',
    'string', 'large_string', 'binary', 'fixed_size_binary[3]', 'decimal128(12, 2)', 'decimal256(40, 4)',
    'date32[day]', 'timestamp[us, tz=UTC]', 'time32[ms]', 'time64[us]', 'duration[us]',
    'list<element: int64>', 'struct<count: int64, label: string>', 'map<string, int64>',
    'dictionary<values=string, indices=int32, ordered=0>', 'null', 'int | str | null'])(
    'preserves column type %s verbatim', type => {
      expect(artifactSchema({ type: 'table', columns: [{ name: 'value', type }] }).columns[0]?.type).toBe(type)
    },
  )
  it('renders actual JSON field types and list element types', () => {
    expect(artifactSchema({ type: 'dict', columns: [{ name: 'ready', type: 'bool' }] })).toMatchObject({ fields: true, columns: [{ name: 'ready', type: 'bool' }] })
    expect(artifactSchema({ type: 'list', rows: 3, item_types: ['int', 'str', 'null'] }).itemTypes).toEqual(['int', 'str', 'null'])
  })
  it.each([null, [], 'bad', 1, false])('handles missing or malformed shapes: %s', shape => {
    expect(artifactSchema(shape)).toMatchObject({ type: null, columns: [], itemTypes: [] })
  })
})
