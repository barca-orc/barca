import { describe, expect, it } from 'vitest'
import { renderToStaticMarkup } from 'react-dom/server'
import { RefreshPathLine } from './RefreshPathLine'
import type { RefreshPath } from '@/lib/generated/RefreshPath'

const text = (html: string) => html.replace(/<[^>]+>/g, '')
const NOW = Date.parse('2026-11-17T14:57:00Z')

const line = (path: Partial<RefreshPath> | null) =>
  text(renderToStaticMarkup(
    <RefreshPathLine
      path={path && { schedule: 'pipeline.py:inbox', next_tick: '2026-11-17T15:00:00Z', state: 'on_time', paused: false, live: true, missed_ticks: 0, ...path }}
      nowMs={NOW}
    />,
  ))

describe('RefreshPathLine', () => {
  it('names the schedule and its next tick', () => {
    expect(line({})).toBe('refresh paththe schedule of inbox · next tick in 3m')
  })

  it('says what the server says of the schedule, one field at a time', () => {
    expect(line({ paused: true, next_tick: null })).toContain('the schedule of inbox · paused')
    expect(line({ live: false, next_tick: null })).toContain('the schedule of inbox · not firing')
    expect(line({ state: "held", live: false, next_tick: null })).toBe("refresh paththe schedule of inbox · held")
    expect(line({ missed_ticks: 1 })).toContain('next tick in 3m · missed 1 tick')
    expect(line({ missed_ticks: 4 })).toContain('missed 4 ticks')
    expect(line({})).not.toContain('missed')
  })

  it('says so when no tick will refresh the node', () => {
    expect(line(null)).toContain('no refresh path')
  })
})
