import { describe, it, expect } from 'vitest'
import { replay } from './program'
import { canLoadMore, INITIAL_RUNS, RUNS_MAX, updateRuns, type RunsMsg } from './runsPage'

const run = (...msgs: RunsMsg[]) => replay(updateRuns, INITIAL_RUNS, msgs)

describe('runs program', () => {
  it('selects and closes through the URL, replacing a resolved handle', () => {
    expect(run({ type: 'run_selected', id: 'r1' }, { type: 'run_resolved', id: '42' }, { type: 'panel_closed' }).effects).toEqual([
      { type: 'navigate', run: 'r1', replace: false },
      { type: 'navigate', run: '42', replace: true },
      { type: 'navigate', run: null, replace: false },
    ])
  })

  it('loads more runs a page at a time, up to the cap', () => {
    expect(run({ type: 'load_more' }).model.limit).toBe(200)
    const capped = run(...Array.from({ length: 20 }, (): RunsMsg => ({ type: 'load_more' }))).model
    expect(capped.limit).toBe(RUNS_MAX)
    expect(canLoadMore(capped)).toBe(false)
  })

  it('retries the request that failed', () => {
    expect(run({ type: 'retry_list' }, { type: 'retry_detail' }).effects).toEqual([{ type: 'refetch_list' }, { type: 'refetch_detail' }])
  })
})
