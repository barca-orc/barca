import { describe, expect, it } from 'vitest'
import { signalLine } from './dataSignals'

describe('signalLine', () => {
  it('says each signal in one line, with its numbers', () => {
    expect(signalLine({ kind: 'empty', empty_keys: 3, keys: 1205, previous_empty_keys: 0 })).toBe('3 of 1,205 keys empty, previously 0')
    expect(signalLine({ kind: 'empty', empty_keys: 2, keys: 4, previous_empty_keys: null })).toBe('2 of 4 keys empty')
    expect(signalLine({ kind: 'empty', empty_keys: 1, keys: 1, previous_empty_keys: 0 })).toBe('empty output (had rows before)')
    expect(signalLine({ kind: 'rows_changed', changed_keys: 1, key: null, rows: 300, previous_rows: 1200 })).toBe('rows changed: 1,200 rows → 300')
    expect(signalLine({ kind: 'rows_changed', changed_keys: 4, key: 'region=r1', rows: 0, previous_rows: 1 })).toBe('rows changed on region=r1: 1 row → 0 (4 keys changed)')
    expect(
      signalLine({ kind: 'behind_input', input: 'p.py:orders', input_success_at: '2026-11-16 09:00:00', last_success_at: '2026-11-14 10:00:00' }),
    ).toBe('older than its input orders')
  })
})
