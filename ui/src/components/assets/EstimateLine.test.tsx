import { describe, expect, it } from 'vitest'
import { renderToStaticMarkup } from 'react-dom/server'
import { EstimateLine } from './EstimateLine'

const text = (html: string) => html.replace(/<[^>]+>/g, '')
const NOW = Date.parse('2026-11-17T14:57:00Z')

const line = (times: { stale?: string | null, fresh?: string | null, recheck?: string | null }) =>
  text(renderToStaticMarkup(
    <EstimateLine
      staleSince={times.stale ?? null}
      expectedFreshAt={times.fresh ?? null}
      recheckAt={times.recheck ?? null}
      nowMs={NOW}
    />,
  ))

describe('EstimateLine', () => {
  it('says fresh only of an estimate, as an estimate', () => {
    const shown = line({ stale: '2026-11-17T14:55:00Z', fresh: '2026-11-17T15:00:20Z', recheck: '2026-11-17T15:00:00Z' })
    expect(shown).toContain('out of datesince 2m ago')
    expect(shown).toContain('expected freshabout in 3m')
    expect(shown).not.toContain('rechecked')
  })

  it('says rechecked where the server promises no fresh time', () => {
    const shown = line({ recheck: '2026-11-17T15:00:00Z' })
    expect(shown).toContain('rechecked')
    expect(shown).toContain('in 3m')
    expect(shown).not.toContain('fresh')
  })

  it('shows nothing for a node with no time', () => {
    expect(line({})).toBe('')
  })
})
