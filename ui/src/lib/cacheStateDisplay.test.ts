import { describe, expect, it } from 'vitest'
import { CACHE_STATES, cacheStateDisplay, lastAttemptFailed } from './cacheStateDisplay'
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
    expect(cacheStateDisplay('cached').tone).toBe('success')
    expect(cacheStateDisplay('stale').tone).toBe('warning')
    expect(cacheStateDisplay('never_run').tone).toBe('neutral')
  })

  it('draws each state with a glyph of its own, though tones are shared', () => {
    const glyphs = CACHE_STATES.map((state) => cacheStateDisplay(state).glyph)
    // One exception: a node that always runs shows its last run, `succeeded`, with the success check.
    expect(new Set(glyphs).size).toBe(CACHE_STATES.length - 1)
    expect(cacheStateDisplay('always_runs')).toMatchObject({ label: 'succeeded', glyph: cacheStateDisplay('cached').glyph })
    for (const glyph of glyphs) expect(GLYPHS).toContain(glyph)
    // The warnings (blocked, stale, partial, unsynced) share a tone, never a glyph or a hue.
    const warnings = CACHE_STATES.filter((s) => cacheStateDisplay(s).tone === 'warning')
    expect(new Set(warnings.map((s) => glyphColor(cacheStateDisplay(s).glyph))).size).toBe(warnings.length)
  })

  it('reads a failed last attempt as the server says it: a failing health', () => {
    expect(lastAttemptFailed({ health: 'failing' })).toBe(true)
    expect(lastAttemptFailed({ health: 'blocked' })).toBe(false)
    expect(lastAttemptFailed({ health: 'ok' })).toBe(false)
  })
})
