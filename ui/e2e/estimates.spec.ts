import { expect, test } from '@playwright/test'
import { served } from './helpers/served'

// The Schedules page over stubbed answers: what it says of a schedule is the server's
// `schedule` block (`late`, `queue_bound`, the typical wait and run), never worked out here.
const assets = ['pulse', 'nightly', 'hourly'].map((name) => ({
  id: `jobs.py:${name}`, kind: 'sensor', inputs: [], freshness: 'schedule', schedule: name === 'pulse' ? '*/10 * * * * *' : name === 'hourly' ? '0 * * * *' : '0 6 * * *', env: [],
}))
const stands = (over: Record<string, unknown>) => ({
  cron: '* * * * *', next_fire: null, last_fired: null, last_status: null, last_run_id: null, late: false, late_since: null,
  failures: 0, max_failures: 5, failing: false, paused: false, paused_until: null, skip_reason: null, live: true,
  typical_queue_wait_seconds: 0, typical_run_seconds: null, queue_bound: false, ...over,
})
const soon = Math.floor(Date.now() / 1000) + 600
const state = (name: string, schedule: Record<string, unknown>, next: number) => served({
  id: `jobs.py:${name}`, name, kind: 'sensor', inputs: [], partitioned: false, shape: null, env: [], durations: null,
  next_run: next, prev_run: null, late_since: null, running: null, last_materialization: null, signals: [],
  last_success_at: null, failing_since: null, consecutive_failures: 0,
  cache: { state: 'always_runs', reason: 'sensor', detail: '' }, schedule: stands(schedule),
})
const states = [
  state('hourly', { typical_run_seconds: 42 }, soon),
  // Slots passed since its last tick, and the server calls it late.
  state('nightly', { late: true, late_since: new Date(Date.now() - 45 * 60_000).toISOString(), last_fired: new Date(Date.now() - 105 * 60_000).toISOString(), last_status: 'success', last_run_id: 'r1' }, soon + 60),
  // Its ticks wait longer than they are apart: the server's word, and its numbers.
  state('pulse', { queue_bound: true, typical_queue_wait_seconds: 12, typical_run_seconds: 1 }, soon + 120),
]

test('the Schedules page reads late and queue-bound from the server, lists them first, and says the typical wait as an estimate', async ({ page }) => {
  await page.route('**/assets', (route) => route.fulfill({ json: assets }))
  await page.route('**/groups', (route) => route.fulfill({ json: [] }))
  await page.route(/\/state(?:\?.*)?$/, (route) => route.fulfill({ json: states }))
  await page.route(/\/schedule$/, (route) => route.fulfill({ json: [] }))
  await page.goto('/ui/#/schedules')
  const rows = page.locator('.barca-schedule-table tbody tr')
  await expect(rows).toHaveCount(3)
  // Late first, then queue-bound, then the next slot.
  await expect(rows.nth(0)).toContainText('nightly')
  await expect(rows.nth(0)).toContainText(/late by 4\dm/)
  await expect(rows.nth(1)).toContainText('pulse')
  await expect(rows.nth(1).locator('[data-queue-bound]')).toContainText('queue-bound')
  await expect(rows.nth(1).locator('[data-queue-bound]')).toContainText('raise max_runs or space the schedules')
  await expect(rows.nth(1).locator('[data-queue-wait]')).toHaveText('~15s · <5s')
  await expect(rows.nth(2)).toContainText('hourly')
  await expect(rows.nth(2)).toContainText('on time')
  await expect(rows.nth(2).locator('td.col-wait')).toHaveText('none · ~45s')
  await expect(page.locator('.barca-view-actions')).toContainText('1 late · 1 queue-bound')
  // A row the server does not call late is never late here, whatever its times say.
  await expect(rows.nth(1)).not.toContainText('late by')
})

test("a kind's page: the chips are the health map's parts, their counts the rollup's, and a chip lists the rows the server gave that cause", async ({ page, request }) => {
  const api = process.env.BARCA_E2E_API
  const rollup = await (await request.get(`${api}/state/rollup`)).json()
  const states: { kind: string; cause: string | null }[] = await (await request.get(`${api}/state`)).json()
  const kind = rollup.kinds.task
  // The rollup counts the same nodes the page lists (#992).
  expect(kind.nodes).toBe(states.filter((s) => s.kind === 'task').length)
  expect(kind.nodes).toBeGreaterThan(0)
  const served: Record<string, number> = { ...kind.causes, fresh: kind.fresh }
  await page.goto('/ui/#/tasks')
  const chips = page.getByRole('group', { name: 'State' })
  for (const [part, count] of Object.entries(served)) {
    const dot = chips.locator(`.barca-chip-dot[data-part="${part}"]`)
    if (count === 0) {
      await expect(dot).toHaveCount(0)
      continue
    }
    await expect(dot).toHaveCount(1)
    await expect(chips.locator(`button:has(.barca-chip-dot[data-part="${part}"]) b`)).toHaveText(count.toLocaleString('en-US'))
  }
  // No chip speaks the cache-state vocabulary.
  await expect(chips).not.toContainText(/stale|never run|cached|blocked/)
  // Picking a chip lists exactly the rows with that cause, and the URL says so.
  const [part, count] = Object.entries(served).find(([, n]) => n > 0)!
  await chips.locator(`button:has(.barca-chip-dot[data-part="${part}"])`).click()
  await expect(page).toHaveURL(new RegExp(`state=${part}`))
  await expect(page.locator('.barca-asset-table tbody tr[tabindex="0"]')).toHaveCount(count)
  // An older link still opens: `failed=1` is the failing cause.
  await page.goto('/ui/#/tasks?failed=1')
  await expect(page.locator('.barca-asset-table tbody tr[tabindex="0"]')).toHaveCount(served.failing ?? 0)
})
