import { describe, expect, it } from 'vitest'
import type { AssetRunEntry, NodeState } from './types'
import { downstreamOf, formatBytes, historyBars, shortHash } from './nodeDetail'

function node(id: string, inputs: string[] = []): NodeState {
  return {
    id,
    name: id.split(':').pop()!,
    kind: 'asset',
    inputs,
    partitioned: false,
    cache: { state: 'cached', reason: 'materialized', detail: '' },
    last_materialization: null,
    shape: null,
    env: [],
    durations: null,
    next_run: null,
  }
}

describe('downstreamOf', () => {
  it('lists the nodes that read this one, sorted by name', () => {
    const nodes = [
      node('a.py:src'),
      node('b.py:zeta', ['a.py:src']),
      node('a.py:alpha', ['a.py:src', 'a.py:other']),
      node('a.py:other'),
    ]
    expect(downstreamOf('a.py:src', nodes).map((n) => n.name)).toEqual(['alpha', 'zeta'])
    expect(downstreamOf('b.py:zeta', nodes)).toEqual([])
  })
})

describe('historyBars', () => {
  const run = (status: string, elapsed: number | null, at: string): AssetRunEntry => ({
    status,
    elapsed_seconds: elapsed,
    created_at: at,
    error_message: status === 'failed' ? 'boom' : null,
    attempts: 1,
  })

  it('oldest first, heights relative to the slowest run, failures marked', () => {
    // The API returns newest first.
    const bars = historyBars([
      run('success', 10, '2026-10-06 03:00:00'),
      run('failed', null, '2026-10-06 02:00:00'),
      run('success', 5, '2026-10-06 01:00:00'),
    ])
    expect(bars.map((b) => b.createdAt)).toEqual([
      '2026-10-06 01:00:00',
      '2026-10-06 02:00:00',
      '2026-10-06 03:00:00',
    ])
    expect(bars.map((b) => b.heightPct)).toEqual([50, 100, 100])
    expect(bars.map((b) => b.failed)).toEqual([false, true, false])
  })

  it('a failure with no duration still shows as a full-height failed bar', () => {
    const bars = historyBars([run('failed', null, '2026-10-06 01:00:00')])
    expect(bars).toEqual([
      { createdAt: '2026-10-06 01:00:00', heightPct: 100, failed: true, label: 'failed' },
    ])
  })
})

describe('formatting', () => {
  it('formats byte sizes', () => {
    expect(formatBytes(512)).toBe('512 B')
    expect(formatBytes(2048)).toBe('2.0 KB')
    expect(formatBytes(5 * 1024 * 1024)).toBe('5.0 MB')
    expect(formatBytes(3.5 * 1024 ** 3)).toBe('3.5 GB')
  })
  it('shortens a run hash', () => {
    expect(shortHash('481ebef1bb2c912337781081c42d7185')).toBe('481ebef1bb2c')
  })
})
