import { describe, expect, it } from 'vitest'
import { renderToStaticMarkup } from 'react-dom/server'
import { HealthBar } from './HealthBar'
import { causeCounts, weightsOf } from '@/lib/testdata/rollup'
import type { CauseCounts } from '@/lib/causeDisplay'

const text = (html: string) => html.replace(/<[^>]+>/g, '')
const scope = (nodes: number, causes: Partial<CauseCounts>, not_fresh: number, running = 0) => {
  const counted = causeCounts(causes)
  return { nodes, not_fresh, causes: counted, weights: weightsOf(nodes, counted), running }
}

describe('HealthBar', () => {
  it("is in the health map's words and colours: a segment per cause inside, as wide as its served weight", () => {
    const html = renderToStaticMarkup(<HealthBar scope={scope(85, { failing: 1, catching_up: 84 }, 85)} />)
    expect(text(html)).toBe('1 failing · 84 catching up')
    expect([...html.matchAll(/data-part="([a-z_]+)" style="background:([^;]+);flex-grow:(\d+)/g)].map((m) => m.slice(1))).toEqual([
      ['failing', 'var(--map-failing)', '6'],
      ['catching_up', 'var(--map-catching-up)', '168'],
    ])
    // None of the old health words.
    expect(html).not.toMatch(/behind|never run|blocked|\bok\b/)
  })

  it('says a fully fresh scope is fresh, and names how many causes it left out', () => {
    expect(text(renderToStaticMarkup(<HealthBar scope={scope(116, {}, 0)} />))).toBe('all 116 fresh')
    const many = scope(106, { failing: 4, overdue: 3, no_refresh_path: 7, undecided: 6 }, 20, 2)
    expect(text(renderToStaticMarkup(<HealthBar scope={many} />))).toBe('4 failing · 3 overdue · 2 more · 2 running')
    expect(text(renderToStaticMarkup(<HealthBar scope={many} labelLimit={10} />))).toBe('4 failing · 3 overdue · 7 no refresh path · 6 undecided · 2 running')
    // Beside a running badge the bar does not repeat the run work.
    expect(text(renderToStaticMarkup(<HealthBar scope={many} quiet />))).toBe('4 failing · 3 overdue · 2 more')
    expect(text(renderToStaticMarkup(<HealthBar scope={scope(0, {}, 0)} />))).toBe('empty')
  })
})
