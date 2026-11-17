import { expect, test, type APIRequestContext } from '@playwright/test'
import { isStartRun, seedRuns, startRun, waitForRun } from './helpers/seed'

const API = process.env.BARCA_E2E_API ?? 'http://127.0.0.1:8274'
type HistoryRun = { id: string; target: string | null; status: string }

async function history(request: APIRequestContext): Promise<HistoryRun[]> {
  const response = await request.get(`${API}/runs`)
  expect(response.ok()).toBeTruthy()
  return (await response.json()).runs
}

async function finishedRun(request: APIRequestContext, target: string, verb: 'run' | 'get' = 'run') {
  const before = new Set((await history(request)).map((run) => run.id))
  await waitForRun(request, await startRun(request, target, verb))
  const run = (await history(request)).find((entry) => !before.has(entry.id) && entry.target === target)
  expect(run, `durable history entry for ${target}`).toBeDefined()
  return run!
}

test.describe('run history', () => {
  test('lists real prior runs and opens durable logs and steps after reload', async ({ page, request }) => {
    const before = new Set((await history(request)).map((run) => run.id))
    await seedRuns(request, 'say_hello', 2)
    const seeded = (await history(request)).filter((run) => !before.has(run.id) && run.target === 'say_hello')
    expect(seeded).toHaveLength(2)

    await page.goto('/ui/#/runs')
    const runLink = page.getByRole('button', { name: seeded[0].id, exact: true })
    await expect(runLink).toBeVisible()
    await runLink.click()
    await expect(page).toHaveURL(new RegExp(`#/runs\\?run=${seeded[0].id}`))
    const details = page.getByRole('complementary', { name: 'Run details' })
    await expect(details).toContainText('say_hello')
    await expect(details).toContainText('success')
    await expect(details).toContainText('hello from e2e 6')
    await expect(details.getByRole('heading', { name: 'Steps', exact: true })).toBeVisible()
    await expect(details.locator('.barca-run-nodes')).toContainText('say_hello')

    await page.reload()
    await expect(details).toContainText('hello from e2e 6')
    await expect(details).toContainText('success')
  })

  test('graph asset trigger exposes a live run and refreshes its inspectable details', async ({ page, request }) => {
    const before = new Set((await history(request)).map((run) => run.id))
    await page.goto('/ui/#/graph')
    await page.locator('.react-flow__node[data-id="pipeline.py:history_asset"]').click()
    const started = page.waitForResponse((response) => isStartRun(response.request(), 'get', 'pipeline.py:history_asset'))
    await page.getByRole('complementary', { name: /inspector$/ }).getByRole('button', { name: 'Run', exact: true }).click()
    expect((await started).ok()).toBeTruthy()
    await expect(page).toHaveURL(/#\/assets\?.*view=graph/)
    await page.getByRole('link', { name: 'View run', exact: true }).click()
    await expect(page).toHaveURL(/#\/runs\?run=/)
    const details = page.getByRole('complementary', { name: 'Run details' })
    await expect(details).toContainText('history_asset')
    await expect(details).toContainText(/queued|running/)
    await expect(details).toContainText('history asset started', { timeout: 30_000 })
    await expect(details).toContainText('history asset finished', { timeout: 30_000 })
    await expect(details).toContainText('success')
    await expect(details.getByRole('heading', { name: 'Steps', exact: true })).toBeVisible()
    await expect(details.locator('.barca-run-nodes')).toContainText('history_asset')

    const created = (await history(request)).find((run) => !before.has(run.id) && run.target === 'pipeline.py:history_asset')
    expect(created).toBeDefined()
    await page.goto('/ui/#/runs')
    await expect(page.getByRole('button', { name: created!.id, exact: true })).toBeVisible()
    await page.goto(`/ui/#/runs?run=${created!.id}`)
    await page.reload()
    await expect(details).toContainText('history asset finished')
    await expect(details).toContainText('success')
  })

  test('a cached asset trigger still creates a durable inspectable run', async ({ page, request }) => {
    await finishedRun(request, 'pipeline.py:numbers', 'get')
    const before = new Set((await history(request)).map((run) => run.id))
    await page.goto('/ui/#/graph')
    await page.locator('.react-flow__node[data-id="pipeline.py:numbers"]').click()
    const started = page.waitForResponse((response) => isStartRun(response.request(), 'get', 'pipeline.py:numbers'))
    await page.getByRole('complementary', { name: /inspector$/ }).getByRole('button', { name: 'Run', exact: true }).click()
    expect((await started).ok()).toBeTruthy()
    await page.getByRole('link', { name: 'View run', exact: true }).click()
    const details = page.getByRole('complementary', { name: 'Run details' })
    await expect(details).toContainText('success')
    // Steps are counted in one unit (#776): the node row says it was cached, the summary says 0 ran.
    await expect(details.locator('.barca-run-nodes')).toContainText('numbers')
    await expect(details.locator('.barca-run-nodes')).toContainText('cached')
    await expect(details.locator('.barca-fact').filter({ hasText: /^Steps0 ok · 1 cached/ })).toHaveCount(1)
    const created = (await history(request)).find((run) => !before.has(run.id) && run.target === 'pipeline.py:numbers')
    expect(created).toBeDefined()
    await expect(page.getByRole('button', { name: created!.id, exact: true })).toBeVisible()
    await page.goto(`/ui/#/runs?run=${created!.id}`)
    await page.reload()
    await expect(details.locator('.barca-run-nodes')).toContainText('cached')
    await expect(details).toContainText('success')
    await expect(details.locator('.barca-fact').filter({ hasText: /^Steps0 ok · 1 cached/ })).toHaveCount(1)
    await page.screenshot({ path: '/tmp/barca-ui-runs-details.png', fullPage: true })
  })

  test('failed runs retain their error and logged output', async ({ page, request }) => {
    const run = await finishedRun(request, 'history_failure')
    expect(run.status).toBe('failed')
    await page.goto(`/ui/#/runs?run=${run.id}`)
    const details = page.getByRole('complementary', { name: 'Run details' })
    await expect(details).toContainText('failed')
    await expect(details).toContainText('history failure detail')
    await expect(details).toContainText('history failure logged')
    await page.reload()
    await expect(details).toContainText('history failure detail')
    await expect(details).toContainText('history failure logged')
  })
})

test('a failed run names its first failed step, a bar filters to its hour, and schedule runs hide', async ({ page }) => {
  const started = new Date(Date.now() - 5 * 60_000).toISOString()
  const earlier = new Date(Date.now() - 3 * 3_600_000).toISOString()
  const row = { handle: null, command: 'get', files: [], target: null, triggered_by: null, scheduled_at: null, schedule_id: null, finished_at: null, elapsed_seconds: 1, steps_cached: 0, error: null }
  // The strip is the server's `activity`: 24 hours of UTC counted over every run of the window,
  // here 62 runs of which the list holds two.
  const counts = { success: 0, failed: 0, error: 0, cancelled: 0, interrupted: 0, running: 0 }
  const triggers = { schedule: 0, api: 0, manual: 0, client: 0, unknown: 0 }
  const thisHour = Math.floor(Date.now() / 3_600_000) * 3_600_000
  const startedHour = Math.floor(Date.parse(started) / 3_600_000) * 3_600_000
  const activityOf = (schedule: number) => {
    const hours = Array.from({ length: 24 }, (_, i) => {
      const at = thisHour - (23 - i) * 3_600_000
      const failed = at === startedHour ? 1 : 0
      const ok = at === startedHour ? schedule : 0
      return { at: new Date(at).toISOString().replace('.000Z', 'Z'), runs: ok + failed, ok, not_successful: failed, in_progress: 0,
        by_status: { ...counts, success: ok, failed }, by_trigger: { ...triggers, schedule: ok, manual: failed } }
    })
    const total = { ...hours[0]!, runs: schedule + 1, ok: schedule, not_successful: 1 }
    return { since: hours[0]!.at, until: new Date(thisHour + 3_600_000).toISOString(), triggered_by: schedule ? [] : ['manual', 'api', 'client'], hours, total,
      summary: `${schedule + 1} runs in the last 24 hours · 1 not successful` }
  }
  const asked: string[] = []
  await page.route(/\/runs(\?.*)?$/, (route) => {
    const hidden = new URL(route.request().url()).searchParams.get('triggered_by')
    asked.push(hidden ?? '')
    const runs = [
      { ...row, id: 'r2', run_id: 'r2', status: 'failed', triggered_by: 'manual', started_at: started, steps_total: 28404, steps_executed: 21950, first_failure: { node_id: 'p.py:orders', error: "KeyError: 'currency'" },
        // The row's words are the server's (`line`).
        line: { steps: '21,950 ok · 0 cached', steps_detail: 'Steps are keys: a partitioned node counts one per key · 28,404 nodes planned · stopped at p.py:orders', failure: "stopped at orders · KeyError: 'currency'", failure_detail: "p.py:orders: KeyError: 'currency'" } },
      { ...row, id: 'r1', run_id: 'r1', status: 'success', triggered_by: 'schedule', started_at: earlier, steps_total: 1, steps_executed: 1, first_failure: null, line: { steps: '1 ok · 0 cached', steps_detail: 'Steps are keys: a partitioned node counts one per key · 1 node planned', failure: null, failure_detail: null } },
    ].filter((run) => !hidden || hidden.split(',').includes(run.triggered_by))
    return route.fulfill({ json: { runs, total: hidden ? 1 : 62, truncated: !hidden, activity: activityOf(hidden ? 0 : 61) } })
  })
  await page.goto('/ui/#/runs')
  const failed = page.getByRole('row', { name: /r2/ })
  await expect(failed).toContainText("stopped at orders · KeyError: 'currency'")
  await expect(failed).toContainText('21,950 ok · 0 cached')
  await expect(failed).toContainText('stopped at orders')
  await expect(failed).toContainText('5m ago')
  await expect(page.locator('.barca-activity-now')).toHaveText('now')
  // Counted over the window, not the two runs loaded.
  await expect(page.locator('.barca-activity-summary')).toContainText('62 runs · 1 not successful')
  await page.getByRole('button', { name: /62 runs, 1 not successful/ }).click()
  await expect(page.getByRole('row', { name: /r1/ })).toHaveCount(0)
  await expect(failed).toBeVisible()
  // Back to every hour, then without the schedule's runs: the server filters the list and the strip.
  await page.getByRole('button', { name: /62 runs, 1 not successful/ }).click()
  await expect(page.getByRole('row', { name: /r1/ })).toBeVisible()
  await page.getByRole('button', { name: 'Hide schedule runs' }).click()
  await expect(page.getByRole('row', { name: /r1/ })).toHaveCount(0)
  await expect(failed).toBeVisible()
  await expect(page.locator('.barca-activity-summary')).toContainText('1 run · 1 not successful · schedule runs hidden')
  await expect(page).toHaveURL(/schedule=hide/)
  expect(asked).toContain('manual,api,client')
})

// A deep queue (the fixture's runs finish too fast to make one): the running run is listed first,
// the list scrolls in a box of its own, and the run history keeps most of the page.
test('a deep queue lists its running run first and leaves the history the page', async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  const at = (minutes: number) => new Date(Date.now() - minutes * 60_000).toISOString()
  const entry = (n: number) => ({ run_id: `q${n}`, status: 'pending', position: n, command: 'get', target: `pipeline.py:t${n}`, triggered_by: 'api', schedule_id: null, queued_at: at(20 - n / 2) })
  await page.route(/\/queue$/, (route) => route.fulfill({ json: {
    paused: false, paused_at: null, draining: false, max_runs: 1, capacity: null,
    running: [{ ...entry(0), run_id: 'live', status: 'running', position: null, target: 'pipeline.py:slow', queued_at: at(25) }],
    queued: Array.from({ length: 18 }, (_, i) => entry(i + 1)),
  } }))
  await page.goto('/ui/#/runs')
  const queue = page.getByRole('region', { name: 'Queue' })
  const rows = queue.getByRole('table', { name: 'Queued runs' }).locator('tbody tr')
  await expect(rows).toHaveCount(19)
  await expect(rows.first()).toContainText('running')
  await expect(rows.first()).toContainText('live')
  await expect(rows.first()).toContainText('25m')
  await expect(rows.nth(1)).toContainText('#1')
  const history = page.locator('.barca-runs-history')
  expect((await queue.boundingBox())!.height).toBeLessThan(340)
  expect((await history.boundingBox())!.height).toBeGreaterThan(450)
  // Every page says the queue in brief, and it opens Runs.
  await expect(page.getByRole('link', { name: /^Queue: 1 running · 18 waiting · oldest \d+m$/ })).toBeVisible()

  await queue.getByRole('button', { name: 'Hide the queued runs' }).click()
  await expect(rows).toHaveCount(0)
  await expect(queue).toContainText(/oldest waiting \d+m · next pipeline\.py:t1/)
  await queue.getByRole('button', { name: 'Show the queued runs' }).click()
  await expect(rows).toHaveCount(19)
})

// A run of thousands of keys: the logs and the steps render what is in view, not every line.
test('a run of 30,000 log lines and 1,000 keys renders only what is in view', async ({ page }) => {
  const key = (i: number) => `account=acct_${String(i).padStart(6, '0')}`
  const step = (i: number) => ({
    node_id: `p.py:invoices[${key(i)}]`, status: i < 250 ? 'failed' : 'success', elapsed_seconds: 0.1,
    error: i < 250 ? 'ValueError: negative total' : null, partition_key: key(i), attempts: 1, run_hash: null,
    artifact_path: null, artifact_format: null, artifact_size_bytes: null, error_type: null, error_message: null,
    error_traceback: null, error_phase: null, worker_exit: null, cpu_seconds: null, max_rss_bytes: null, output_tail: null, batch: null,
  })
  const started = new Date(Date.now() - 60_000).toISOString()
  await page.route(/\/runs\/big$/, (route) => route.fulfill({ json: {
    run_id: 'big', status: 'failed', command: 'get', target: null, files: [], triggered_by: 'api', scheduled_at: null, schedule_id: null,
    started_at: started, finished_at: started, elapsed_seconds: 13, steps_total: 7644, steps_executed: 5000, steps_cached: 0,
    job: 'all', attempts: null, client_run_id: null, failure: null, outcome: null, error: 'invoices: 1,250 keys failed',
    explanation: {
      targets: [], requested_by: 'api', attempts: null, steps: '3,750 ok · 1,250 failed · 0 cached', target_outcomes: [], cancellation: null,
      why: { headline: '5,000 steps ran, but this run recorded no reasons (barca records them from 0.43).', groups: [], omitted: 0, partial: false },
      progress: null, slowest: [],
    },
    steps: Array.from({ length: 1000 }, (_, i) => step(i)),
    steps_page: { total: 5000, truncated: true, nodes: [{ node_id: 'p.py:invoices', keys: 5000, failed: 1250, elapsed_seconds: 58, calls: 0, status: 'ran' }] },
    logs: Array.from({ length: 30_000 }, (_, seq) => ({ node_id: `p.py:invoices[${key(Math.floor(seq / 6))}]`, seq, line: `line ${seq + 1}`, stream: 'stdout', ts: started })),
  } }))
  await page.goto('/ui/#/runs?run=big')
  const details = page.getByRole('complementary', { name: 'Run details' })
  // Pinned to the end, as a live log is.
  await expect(details.locator('.barca-log-line').last()).toContainText('line 30000')
  expect(await details.locator('.barca-log-line').count()).toBeLessThan(100)
  // The node's 1,000 listed keys are one row with their count until it is opened.
  const steps = details.locator('.barca-run-nodes')
  await expect(steps.locator('.barca-run-row')).toHaveCount(1)
  await expect(steps).toContainText('5,000 keys · 1,250 failed')
  await steps.getByRole('button', { name: /invoices/ }).click()
  await expect(steps).toContainText('account=acct_000000')
  expect(await steps.locator('.barca-run-row').count()).toBeLessThan(60)
  expect(await page.locator('*').count()).toBeLessThan(5000)
})
