import { renderToStaticMarkup } from 'react-dom/server'
import { describe, expect, it } from 'vitest'
import { Age } from './Age'

const NOW = Date.parse('2026-10-30T12:00:00Z')
const text = (html: string) => html.replace(/<[^>]+>/g, '')

describe('Age', () => {
  it('is relative to the data it came with', () => {
    expect(text(renderToStaticMarkup(<Age at="2026-10-30 09:00:00" nowMs={NOW} />))).toBe('3h ago')
  })

  // The panel is drawn before its query answers: `nowMs` is the query's `dataUpdatedAt`, 0 until
  // then. The time is read against the clock instead, never as `in 20774d` (#970).
  it('reads against the clock while its data is loading', () => {
    // Half an hour past three: the shared clock ticks every 15 s.
    const at = new Date(Date.now() - 3.5 * 3_600_000).toISOString()
    expect(text(renderToStaticMarkup(<Age at={at} nowMs={0} />))).toBe('3h ago')
  })

  it('shows an absent or zero time as a dash, with no <time>', () => {
    expect(renderToStaticMarkup(<Age at={null} nowMs={NOW} />)).toBe('–')
    expect(renderToStaticMarkup(<Age at={0} nowMs={NOW} />)).toBe('–')
    expect(renderToStaticMarkup(<Age at="1970-01-01 00:00:00" nowMs={NOW} />)).toBe('–')
  })
})
