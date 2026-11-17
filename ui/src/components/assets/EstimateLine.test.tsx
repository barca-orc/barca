import { beforeAll, describe, expect, it } from 'vitest'
import { renderToStaticMarkup } from 'react-dom/server'
import { EstimateLine, type EstimateLineProps } from './EstimateLine'
import { CauseLine } from './CauseLine'
import { heldRoot } from '@/lib/testdata/rollup'

const text = (html: string) => html.replace(/<[^>]+>/g, '')
const NOW = Date.parse('2026-11-17T14:57:00Z')

// The panel shows times in the reader's zone; this file's reader is in New York (UTC-5).
beforeAll(() => {
  process.env.TZ = 'America/New_York'
})

const line = (times: { stale?: string | null, fresh?: string | null, recheck?: string | null, why?: EstimateLineProps['noEstimate'] }) =>
  text(renderToStaticMarkup(
    <EstimateLine
      staleSince={times.stale ?? null}
      expectedFreshAt={times.fresh ?? null}
      recheckAt={times.recheck ?? null}
      noEstimate={times.why ?? null}
      nowMs={NOW}
    />,
  ))

describe('EstimateLine', () => {
  it('says fresh only of an estimate, as a time with ~ rounded up to the minute', () => {
    const shown = line({ stale: '2026-11-17T14:55:00Z', fresh: '2026-11-17T15:00:20Z', recheck: '2026-11-17T15:00:00Z' })
    expect(shown).toContain('out of datesince 2m ago')
    expect(shown).toContain('fresh~10:01 · in 3m, an estimate')
    expect(shown).not.toContain('rechecked')
    expect(shown).not.toContain('10:00:20')
  })

  it('says rechecked, at the exact slot, where the server promises no fresh time, and why there is no estimate', () => {
    const shown = line({ recheck: '2026-11-17T15:00:00Z', why: 'not_promised' })
    expect(shown).toContain('rechecked10:00 · in 3m')
    expect(shown).toContain('no estimatea result above it may come out unchanged')
    expect(shown).not.toContain('fresh')
  })

  it('says why a node nothing refreshes has no estimate, not a blank', () => {
    expect(line({ why: 'no_path' })).toBe('no estimateno refresh path')
    expect(line({ why: 'paused' })).toBe('no estimatepaused')
  })

  it('shows nothing for a fresh node with no time', () => {
    expect(line({})).toBe('')
  })
})

describe('CauseLine', () => {
  const root = { id: 'code:market.py:prices', kind: 'code' as const, node: 'market.py:prices' }
  it("names the cause in the map's word and the root with a link to its node", () => {
    const html = renderToStaticMarkup(<CauseLine node="market.py:prices_out_3" cause="catching_up" root={root} onSelect={() => {}} />)
    expect(text(html)).toBe('whycatching upcode change in prices')
    expect(html).toContain('title="Open market.py:prices"')
    expect(html).toContain('data-cause="catching_up"')
  })

  it("adds the root's fact and what clears it once the root list has loaded", () => {
    const held = heldRoot({ kind: 'no_schedule', node: 'ledger.py:ledger', detail: 'no tick reaches ledger', nodes: 152, action: { kind: 'add_schedule', command: null, estimated_seconds: null } })
    const shown = text(renderToStaticMarkup(<CauseLine node="ledger.py:ledger" cause="no_refresh_path" root={{ id: held.id, kind: held.kind, node: held.node }} held={held} onSelect={() => {}} />))
    // The root is the node itself: no link to itself.
    expect(shown).toContain('no refresh pathno schedule here')
    expect(shown).toContain('no tick reaches ledger')
    expect(shown).toContain('to clear it: add a schedule freshness=Schedule(...) on it or on a node above it')
  })

  it('is nothing for a fresh node', () => {
    expect(renderToStaticMarkup(<CauseLine node="a.py:x" cause={null} root={null} onSelect={() => {}} />)).toBe('')
  })
})
