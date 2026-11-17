import { describe, expect, it } from 'vitest'
import { renderToStaticMarkup } from 'react-dom/server'
import { LastCheckLine } from './LastCheckLine'
import type { LastCheck } from '@/lib/generated/LastCheck'

const text = (html: string) => html.replace(/<[^>]+>/g, '')
const NOW = Date.parse('2026-11-17T15:03:20Z')

const line = (outcome: LastCheck['outcome'], at = '2026-11-17T15:00:05Z') =>
  text(renderToStaticMarkup(
    <LastCheckLine check={{ at, outcome, schedule: 'pipeline.py:clock' }} nowMs={NOW} />,
  ))

describe('LastCheckLine', () => {
  it('reads as the tick found the node', () => {
    expect(line('up_to_date')).toContain('checked 3m ago, up to date')
    expect(line('rebuilt')).toContain('checked 3m ago, rebuilt')
    expect(line('failed')).toContain('checked 3m ago, failed')
    expect(line('not_reached')).toContain('checked 3m ago, not reached')
  })

  it('names the schedule whose tick it was, and keeps an old check as it is', () => {
    expect(line('up_to_date')).toContain('by the schedule of clock')
    expect(line('up_to_date', '2026-11-11T03:00:00Z')).toContain('checked 6d ago, up to date')
  })
})
