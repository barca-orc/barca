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

// The page's filters are the server's (#969): each is a query of `GET /runs`, and the list, its
// count and the strip are what comes back. The route here answers as the server does for three
// runs among 61 schedule ticks, so the test can read what the page asked for.
test('the default view hides schedule runs, and every filter asks the server', async ({ page }) => {
  const HOUR = 3_600_000
  const thisHour = Math.floor(Date.now() / HOUR) * HOUR
  const stamp = (ms: number) => new Date(ms).toISOString().replace('.000Z', 'Z')
  const row = { handle: null, command: 'get', files: [], scheduled_at: null, schedule_id: null, finished_at: null, elapsed_seconds: 1, steps_cached: 0, error: null, first_failure: null }
  const line = (steps: string, failure: string | null = null) => ({ steps, steps_detail: 'Steps are keys: a partitioned node counts one per key', failure, failure_detail: failure })
  const all = [
    { ...row, id: 'r2', run_id: 'r2', target: 'orders', status: 'failed', triggered_by: 'manual', started_at: stamp(thisHour - HOUR + 60_000), steps_total: 2, steps_executed: 1, line: line('0 ok · 1 failed · 0 cached', "stopped at orders · KeyError: 'currency'") },
    // Recorded before barca stored what started a run: never hidden with the schedule's.
    { ...row, id: 'r0', run_id: 'r0', target: 'legacy_report', status: 'success', triggered_by: null, started_at: stamp(thisHour - 2 * HOUR + 60_000), steps_total: 1, steps_executed: 1, line: line('1 ok · 0 cached') },
    { ...row, id: 'r1', run_id: 'r1', target: 'poll_inbox', status: 'success', triggered_by: 'schedule', started_at: stamp(thisHour - 3 * HOUR + 60_000), steps_total: 1, steps_executed: 1, line: line('1 ok · 0 cached') },
  ]
  const none = { success: 0, failed: 0, error: 0, cancelled: 0, interrupted: 0, running: 0 }
  const nobody = { schedule: 0, api: 0, manual: 0, client: 0, unknown: 0 }
  const entry = (at: number, runs: number, bad: number, schedule: number) => ({
    at: stamp(at), until: stamp(at + HOUR), runs, ok: runs - bad, not_successful: bad, in_progress: 0,
    by_status: { ...none, success: runs - bad, failed: bad }, by_trigger: { ...nobody, schedule, manual: runs - schedule },
    median_seconds: runs ? 1.5 : null, failure: bad ? { text: "orders: KeyError: 'currency'", runs: bad } : null,
  })
  const asked: URLSearchParams[] = []
  await page.route(/\/runs(\?.*)?$/, (route) => {
    const query = new URL(route.request().url()).searchParams
    asked.push(query)
    const hide = query.get('not_triggered_by') === 'schedule'
    const [since, until, search] = [query.get('since'), query.get('until'), query.get('search')]
    const tickHour = thisHour - HOUR
    const within = (at: number) => (!since || at >= Date.parse(since)) && (!until || at < Date.parse(until))
    // Every filter but the window, which the activity ignores (it is always the last day).
    const keeps = (run: typeof all[number]) =>
      !(hide && run.triggered_by === 'schedule') &&
      (query.get('status') !== 'not_successful' || run.status === 'failed') &&
      (!search || `${run.id} ${run.target}`.includes(search))
    const counted = all.filter(keeps)
    const runs = counted.filter((run) => within(Date.parse(run.started_at)))
    // The ticks are counted, not listed: 61 in the hour of `r2`, while the schedule's runs show.
    const ticks = hide || search || query.get('status') ? 0 : 61
    const hours = Array.from({ length: 24 }, (_, i) => {
      const at = thisHour - (23 - i) * HOUR
      const mine = counted.filter((run) => Date.parse(run.started_at) >= at && Date.parse(run.started_at) < at + HOUR)
      const extra = at === tickHour ? ticks : 0
      return entry(at, mine.length + extra, mine.filter((run) => run.status === 'failed').length, extra + mine.filter((run) => run.triggered_by === 'schedule').length)
    })
    const listed = runs.length + (within(tickHour) ? ticks : 0)
    const sum = (pick: (h: typeof hours[number]) => number) => hours.reduce((n, h) => n + pick(h), 0)
    const total = { ...entry(thisHour - 23 * HOUR, sum((h) => h.runs), sum((h) => h.not_successful), sum((h) => h.by_trigger.schedule)), until: stamp(thisHour + HOUR) }
    return route.fulfill({ json: {
      runs, total: listed, truncated: listed > runs.length,
      activity: {
        since: total.at, until: total.until, triggered_by: [], not_triggered_by: hide ? ['schedule'] : [], statuses: [], search: search ?? null,
        hours, total, excluded: hide ? { ...entry(thisHour - 23 * HOUR, 62, 3, 62), until: total.until } : null,
        summary: `${total.runs} runs in the last 24 hours`,
      },
    } })
  })
  const last = () => asked[asked.length - 1]!
  await page.goto('/ui/#/runs')
  const failed = page.getByRole('row', { name: /r2/ })
  // The default: without the schedule's runs, by exclusion, so the run with no trigger stays.
  await expect(failed).toContainText("stopped at orders · KeyError: 'currency'")
  await expect(page.getByRole('row', { name: /r0/ })).toBeVisible()
  await expect(page.getByRole('row', { name: /r1/ })).toHaveCount(0)
  await expect.poll(() => last().get('not_triggered_by')).toBe('schedule')
  expect(asked.every((query) => query.get('triggered_by') === null)).toBe(true)
  await expect(page.getByRole('button', { name: 'Hide schedule runs' })).toHaveAttribute('aria-pressed', 'true')
  // What that hid is said, with its failures, as the server counted it.
  const hidden = page.locator('.barca-runs-hidden')
  await expect(hidden).toContainText('62 schedule runs in the last 24 hours are hidden')
  await expect(hidden).toContainText('3 not successful')
  // The strip is the server's window, the zone it is shown in stated, the count the server's.
  await expect(page.locator('.barca-activity-title')).toContainText(/Last 24 hours by hour, \S+/)
  await expect(page.locator('.barca-activity-summary')).toContainText('2 runs · 1 not successful · median 1.5s')
  await expect(page.locator('.barca-activity-now')).toHaveText('now')
  await expect(page.locator('.barca-count')).toHaveText(/^2/)

  // A bar's tooltip is the server's hour: counts, median and leading failure. It asks for the hour.
  const bar = page.getByRole('button', { name: /1 run, 1 not successful · median 1\.5s · orders: KeyError: 'currency'/ })
  await bar.click()
  await expect(page.getByRole('row', { name: /r0/ })).toHaveCount(0)
  await expect(failed).toBeVisible()
  await expect.poll(() => last().get('since')).toBe(stamp(thisHour - HOUR))
  await expect.poll(() => last().get('until')).toBe(stamp(thisHour))
  await expect(page).toHaveURL(/since=.*until=/)
  await expect(page.locator('.barca-count')).toHaveText(/^1/)
  await bar.click()
  await expect(page.getByRole('row', { name: /r0/ })).toBeVisible()

  // Shown again: every run, counted over the history (64), and the choice is in the URL.
  await hidden.getByRole('button', { name: 'Show schedule runs' }).click()
  await expect(page.getByRole('row', { name: /r1/ })).toBeVisible()
  await expect(page).toHaveURL(/schedule=show/)
  await expect.poll(() => last().get('not_triggered_by')).toBeNull()
  await expect(page.locator('.barca-count')).toHaveText(/^64/)
  await expect(hidden).toHaveCount(0)
  await expect(page.locator('.barca-run-more')).toContainText('Showing 3 runs of 64')

  // The status chip and the search are the server's too. Missing rows can be the loading
  // state, so wait for the observed query as well as the rows in the server's answer.
  await page.getByRole('button', { name: 'Not successful', exact: true }).click()
  await expect(page.getByRole('row', { name: /r1/ })).toHaveCount(0)
  await expect.poll(() => last().get('status')).toBe('not_successful')
  await expect(failed).toBeVisible()
  await expect(page.locator('.barca-count')).toHaveText(/^1/)
  await page.getByRole('button', { name: 'All', exact: true }).click()
  await page.getByRole('textbox', { name: /Search every run/ }).fill('legacy')
  await expect(page.getByRole('row', { name: /r0/ })).toBeVisible()
  await expect(page.getByRole('row', { name: /r2/ })).toHaveCount(0)
  await expect.poll(() => last().get('search')).toBe('legacy')
  await page.getByRole('textbox', { name: /Search every run/ }).fill('no_such_run')
  await expect(page.getByText('No run matches.')).toBeVisible()
  await page.getByRole('button', { name: 'Clear filters' }).click()
  await expect(page.getByRole('row', { name: /r1/ })).toBeVisible()
})

// The same against the real server: a status, a search and the schedule toggle each change
// what `GET /runs` is asked, and the rows are its answer.
test('a status chip and the search list the runs the server keeps', async ({ page, request }) => {
  const bad = await finishedRun(request, 'history_failure')
  const good = await finishedRun(request, 'say_hello')
  await page.goto('/ui/#/runs')
  await expect(page.getByRole('button', { name: good.id, exact: true })).toBeVisible()
  const asks = (name: string, value: string) => page.waitForResponse((response) => new URL(response.url()).searchParams.get(name) === value && response.url().includes('/runs?'))
  const failedOnly = asks('status', 'not_successful')
  await page.getByRole('button', { name: 'Not successful', exact: true }).click()
  const answer = await (await failedOnly).json()
  await expect(page.getByRole('button', { name: bad.id, exact: true })).toBeVisible()
  await expect(page.getByRole('button', { name: good.id, exact: true })).toHaveCount(0)
  // The header's count is the server's, of every run the filter keeps.
  await expect(page.locator('.barca-count')).toHaveText(new RegExp(`^${answer.total.toLocaleString()}`))
  await page.getByRole('button', { name: 'All', exact: true }).click()
  const searched = asks('search', good.id)
  await page.getByRole('textbox', { name: /Search every run/ }).fill(good.id)
  expect((await (await searched).json()).total).toBe(1)
  await expect(page.locator('.barca-run-table tbody tr')).toHaveCount(1)
  await expect(page.getByRole('button', { name: good.id, exact: true })).toBeVisible()
})

// A deep queue (the fixture's runs finish too fast to make one): the running run is listed first,
// the list scrolls in a box of its own, and the run history keeps most of the page.
test('a deep queue lists its running run first and leaves the history the page', async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  const at = (minutes: number) => new Date(Date.now() - minutes * 60_000).toISOString()
  const entry = (n: number) => ({ run_id: `q${n}`, status: 'pending', position: n, command: 'get', target: `pipeline.py:t${n},pipeline.py:second`, target_line: `pipeline.py:t${n} +1 more`, triggered_by: 'api', schedule_id: null, queued_at: at(20 - n / 2), started_at: null, estimated_start_at: null, wait_seconds: null, waiting_for: null })
  await page.route(/\/queue$/, (route) => route.fulfill({ json: {
    paused: false, paused_at: null, draining: false, max_runs: 1, slots: '1 of 1 running', capacity: null,
    running: [{ ...entry(0), run_id: 'live', status: 'running', position: null, target: 'pipeline.py:slow', target_line: 'pipeline.py:slow', queued_at: at(25) }],
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
