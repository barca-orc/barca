import { describe, expect, it } from 'vitest'
import { CACHE_STATES, cacheStateDisplay, cacheStateRank, lastAttemptFailed, worstCacheState, type CacheState } from './cacheStateDisplay'
import { TONES } from './tone'
import { GLYPHS, glyphColor } from './glyph'

// The eight words of `barca docs status`, as `barca status --json` prints them. The UI shows these
// and no others (DIRECTION.md, "The UI").
const CLI_WORDS = ['cached', 'unsynced', 'stale', 'never_run', 'partial', 'blocked', 'unknown', 'always_runs'] as const

describe('cache state display', () => {
  it('shows each state as the CLI prints it, and knows every state once', () => {
    expect([...CACHE_STATES].sort()).toEqual([...CLI_WORDS].sort())
    expect(new Set(CACHE_STATES).size).toBe(CACHE_STATES.length)
    for (const state of CACHE_STATES) expect(cacheStateDisplay(state).word).toBe(state)
  })

  it('gives each state a tone from the design system', () => {
    for (const state of CACHE_STATES) expect(TONES).toContain(cacheStateDisplay(state).tone)
    expect(cacheStateDisplay('cached').tone).toBe('success')
    expect(cacheStateDisplay('stale').tone).toBe('warning')
    expect(cacheStateDisplay('never_run').tone).toBe('neutral')
  })

  it('draws each state with a glyph of its own, though tones are shared', () => {
    const glyphs = CACHE_STATES.map((state) => cacheStateDisplay(state).glyph)
    expect(new Set(glyphs).size).toBe(CACHE_STATES.length)
    for (const glyph of glyphs) expect(GLYPHS).toContain(glyph)
    // The warnings (blocked, stale, partial, unsynced) share a tone, never a glyph or a hue.
    const warnings = CACHE_STATES.filter((s) => cacheStateDisplay(s).tone === 'warning')
    expect(new Set(warnings.map((s) => glyphColor(cacheStateDisplay(s).glyph))).size).toBe(warnings.length)
  })

  it('ranks what needs attention first, cached last', () => {
    expect(cacheStateRank('blocked')).toBe(0)
    expect(cacheStateRank('cached')).toBe(CACHE_STATES.length - 1)
    expect(worstCacheState(['cached', 'never_run', 'stale'])).toBe('stale')
    expect(worstCacheState([])).toBeNull()
    // Every member of the generated union has a rank (the table is exhaustive by type).
    const states: CacheState[] = [...CLI_WORDS]
    for (const state of states) expect(cacheStateRank(state)).toBeGreaterThanOrEqual(0)
  })

  it('reads a failed last attempt as a fact of its own', () => {
    const failed = { status: 'failed', created_at: 'today', elapsed_seconds: null, run_hash: null, artifact: null, format: null, size_bytes: null, error: 'boom' }
    expect(lastAttemptFailed({ last_materialization: failed })).toBe(true)
    expect(lastAttemptFailed({ last_materialization: { ...failed, status: 'success', error: null } })).toBe(false)
    expect(lastAttemptFailed({ last_materialization: null })).toBe(false)
    // A partitioned node's last materialization is one key: its failed keys count.
    expect(lastAttemptFailed({ last_materialization: null, partitions: { total: 10, cached: 9, missing: 1, missing_keys: [], failed: 1 } })).toBe(true)
  })
})
