import { renderToStaticMarkup } from 'react-dom/server'
import { describe, expect, it } from 'vitest'
import { MiddleName } from './MiddleName'

// A name is one text node: selecting the row copies `demand_forecast`, not `deman\nd_forecast`,
// and a screen reader reads one word. Where the cut goes is CSS (`.barca-mid`).
describe('MiddleName', () => {
  it('renders the name as a single text node', () => {
    const html = renderToStaticMarkup(<MiddleName name="check__risk_review_alerts_01" />)
    expect(html).toBe('<span class="barca-mid" title="check__risk_review_alerts_01">check__risk_review_alerts_01</span>')
  })

  it('puts the id in the tooltip when given one', () => {
    const html = renderToStaticMarkup(<MiddleName name="demand_forecast" title="planning.py:demand_forecast" />)
    expect(html).toContain('title="planning.py:demand_forecast"')
    expect(html).toContain('>demand_forecast<')
  })
})
