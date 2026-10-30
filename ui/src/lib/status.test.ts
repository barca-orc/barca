import { describe, expect, it } from 'vitest'
import { isCheck, nodeKindLabel, statusMeta } from './status'

describe('node kind words', () => {
  it('calls a checking task a check, wherever its file', () => {
    expect(nodeKindLabel({ id: 'billing.py:check__invoices_00', kind: 'task' })).toBe('check')
    expect(nodeKindLabel({ id: 'modeling.py:validate__model', kind: 'task' })).toBe('check')
  })
  it('leaves other tasks and an asset of the same name alone', () => {
    expect(nodeKindLabel({ id: 'p.py:say_hello', kind: 'task' })).toBe('task')
    expect(isCheck({ id: 'p.py:check__raw', kind: 'asset' })).toBe(false)
  })
})

describe('one name per state', () => {
  it('uses the words of the table', () => {
    expect(statusMeta('success').label).toBe('Healthy')
    expect(statusMeta('warning').label).toBe('Stale')
    expect(statusMeta('skipped').label).toBe('Not run')
  })
})
