import { describe, expect, it } from 'vitest'
import type { AssetRunEntry, NodeState } from './types'
import { barSegments, downstreamOf, durationHistogram, editorLink, editorOpens, editorRoots, formatBytes, historyBars, partitionSelection, previewRows, runPace, shortHash, sourcePath, truncateCell, whatChanged } from './nodeDetail'

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
    next_run: null, last_success_at: null, failing_since: null, consecutive_failures: 0,
    running: null,
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
    run_id: null,
    cpu_seconds: null,
    max_rss_bytes: null,
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

const timed = (elapsed: number | null, status = 'success'): AssetRunEntry => ({
  status,
  elapsed_seconds: elapsed,
  created_at: '2026-01-01 00:00:00',
  error_message: null,
  attempts: 1,
  run_id: null,
  cpu_seconds: null,
  max_rss_bytes: null,
})

describe('durationHistogram', () => {
  it('is empty with fewer than two timed successful runs', () => {
    expect(durationHistogram([], null)).toEqual([])
    expect(durationHistogram([timed(1)], 1)).toEqual([])
    expect(durationHistogram([timed(1), timed(null), timed(5, 'failed')], 1)).toEqual([])
  })

  it('bins durations and marks the last attempt', () => {
    const runs = [timed(1), timed(1.1), timed(1.2), timed(5), timed(10)]
    const bins = durationHistogram(runs, 10, 3)
    expect(bins.map((b) => b.count)).toEqual([3, 1, 1])
    expect(bins[2]?.isLast).toBe(true)
    expect(bins.filter((b) => b.isLast)).toHaveLength(1)
    expect(bins[0]?.heightPct).toBe(100)
  })

  it('uses one bin when every run took the same time', () => {
    const bins = durationHistogram([timed(2), timed(2), timed(2)], 2)
    expect(bins).toHaveLength(1)
    expect(bins[0]).toMatchObject({ count: 3, isLast: true })
  })

  it('does not mark a last attempt outside the sampled range', () => {
    const bins = durationHistogram([timed(1), timed(2)], 50)
    expect(bins.some((b) => b.isLast)).toBe(false)
  })
})

describe('whatChanged', () => {
  it('says what moved', () => {
    expect(whatChanged({ change_causes: ['code_changed'] }, true)).toBe('Failed after a code change')
    expect(whatChanged({ change_causes: ['env_changed'] }, true)).toBe('Failed after an env change')
    expect(whatChanged({ change_causes: ['inputs_changed'] }, true)).toBe('Failed with the same code on new input')
    expect(whatChanged({ change_causes: ['inputs_changed'] }, false)).toBe('Same code, new input')
    expect(whatChanged({ change_causes: ['code_changed', 'inputs_changed', 'upstream_stale'] }, false)).toBe('Code and input changes')
    expect(whatChanged({ change_causes: ['upstream_stale'] }, false)).toBe('Waiting on a stale upstream')
    expect(whatChanged({ change_causes: ['inputs_changed', 'upstream_stale'] }, false)).toBe('Waiting on a stale upstream')
  })
  it('stays silent with no recorded cause', () => {
    expect(whatChanged({}, true)).toBeNull()
  })
})

describe('editorLink', () => {
  it('joins the root and line', () => {
    expect(editorLink('/w/my proj', 'a/b.py', 12)).toBe('vscode://file/w/my%20proj/a/b.py:12')
    expect(editorLink(null, '/abs/b.py', 1)).toBe('vscode://file/abs/b.py:1')
    expect(editorLink(null, 'b.py', 1)).toBeNull()
  })
})

describe('previewRows', () => {
  it('tables rows of objects and plain lists', () => {
    expect(previewRows([{ a: 1 }, { a: 'x', b: null }])).toEqual({ columns: ['a', 'b'], rows: [['1', ''], ['x', 'null']] })
    expect(previewRows([1, 2])).toEqual({ columns: ['value'], rows: [['1'], ['2']] })
    expect(previewRows({ a: 1 })).toEqual({ columns: ['a'], rows: [['1']] })
    expect(previewRows('text')).toBeNull()
    expect(previewRows([])).toBeNull()
  })
})

describe('source path and editor roots', () => {
  it('shows a definition relative to the project root', () => {
    expect(sourcePath('/srv/app', '/srv/app/flows/planning.py')).toBe('flows/planning.py')
    expect(sourcePath('/srv/app/', 'planning.py')).toBe('planning.py')
    expect(sourcePath(null, '/srv/app/planning.py')).toBe('/srv/app/planning.py')
  })
  it('reads the configured roots, one per line or comma', () => {
    expect(editorRoots('/home/me/work\n, /srv/app ,\n')).toEqual(['/home/me/work', '/srv/app'])
    expect(editorRoots(null)).toEqual([])
  })
  it('offers the editor link only for a project under a configured root', () => {
    expect(editorOpens('/home/me/work/barca', ['/home/me/work'])).toBe(true)
    expect(editorOpens('/home/me/work', ['/home/me/work/'])).toBe(true)
    // The server's path on another machine opens nothing here.
    expect(editorOpens('/tmp/uxrev', ['/home/me/work'])).toBe(false)
    expect(editorOpens('/home/me/workspace', ['/home/me/work'])).toBe(false)
    expect(editorOpens('/tmp/uxrev', [])).toBe(false)
    expect(editorOpens(null, ['/'])).toBe(false)
  })
})

describe('partitionSelection', () => {
  it('passes a one-dimension key as the flag takes it', () => {
    expect(partitionSelection('account=ra-02541')).toBe('account=ra-02541')
  })
  it('turns a compound key into the JSON object the flag takes', () => {
    expect(partitionSelection('region=emea,tier=key')).toBe('{"region":"emea","tier":"key"}')
    expect(partitionSelection('a=x=y,b=2')).toBe('{"a":"x=y","b":"2"}')
  })
  it('leaves a key it cannot parse alone', () => {
    expect(partitionSelection('odd,shape')).toBe('odd,shape')
  })
})

describe('runPace', () => {
  it('is per batched call, with the count', () => {
    expect(runPace({ calls: 5, keys: 3077, elapsed_seconds: 2.213 })).toBe('0.4s / call · 5 calls')
    expect(runPace({ calls: 1, keys: 3077, elapsed_seconds: 2.213 })).toBe('2.2s / call · 1 call')
  })
  it('is per run when each key ran on its own', () => {
    expect(runPace({ calls: 0, keys: 3077, elapsed_seconds: 2.213 })).toBe('2.2s / run · 3,077 keys')
  })
  it('says nothing without a run, or a run that recorded no time', () => {
    expect(runPace(undefined)).toBeNull()
    expect(runPace({ calls: 0, keys: 0, elapsed_seconds: 0 })).toBeNull()
    expect(runPace({ calls: 0, keys: 7, elapsed_seconds: 0 })).toBeNull()
  })
})

describe('barSegments', () => {
  it('keeps a thin part visible, taking the width from the widest', () => {
    const w = barSegments({ cached: 3070, failed: 7, stale: 0, total: 3077 })
    expect(w.failed).toBe(2)
    expect(w.stale).toBe(0)
    expect(w.cached + w.failed).toBeCloseTo(100, 6)
  })
  it('leaves parts that are wide enough alone', () => {
    expect(barSegments({ cached: 50, failed: 25, stale: 25, total: 100 })).toEqual({ cached: 50, failed: 25, stale: 25 })
    expect(barSegments({ cached: 0, failed: 0, stale: 0, total: 0 })).toEqual({ cached: 0, failed: 0, stale: 0 })
  })
})

describe('truncateCell', () => {
  it('cuts a long value and leaves a short one', () => {
    expect(truncateCell('short')).toEqual({ short: 'short', truncated: false })
    const long = JSON.stringify(Array.from({ length: 52 }, (_, i) => 100 + i))
    const cut = truncateCell(long)
    expect(cut.truncated).toBe(true)
    expect(cut.short).toHaveLength(80)
    expect(cut.short.endsWith('…')).toBe(true)
  })
})
