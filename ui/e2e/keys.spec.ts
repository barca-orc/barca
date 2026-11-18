import { expect, test, type Page } from '@playwright/test'

// The key explorer against fixed answers: a day × region node of six keys, one failed and one
// blocked behind an input, so every control has something to act on. The explorer renders what
// the server serves (#1036): the overview (`.../partitions/overview`) and a page of keys
// (`.../partitions/keys`), which the server filters, orders and counts. `served` below stands in
// for that server: it answers the two routes from one small table of keys, so a test can assert
// both what the page asked for and what it drew from the answer.
const NODE = 'lake.py:events'
const DAYS = ['d1', 'd2', 'd3']
const REGIONS = ['emea', 'apac']
const NAMES: Record<string, string> = { c: 'fresh', f: 'failed', b: 'blocked', s: 'stale', n: 'never_run', p: 'pinned' }
const NEEDS_RUN = 'fbsn'
const ERROR = 'ValueError: no rows for <region> on <day>'

interface Key { day: string; region: string; code: string }

/** The node's keys, day by day: `states` has one character per day × region, the region varying fastest. */
function keysOf(states: string): Key[] {
  return DAYS.flatMap((day, d) => REGIONS.map((region, r) => ({ day, region, code: states[d * REGIONS.length + r]! })))
}

/** Keys as a run takes them: each dimension's values when they form a rectangle, each key otherwise. */
function selectionOf(keys: Key[]): string | null {
  if (keys.length === 0) return null
  const days = DAYS.filter((d) => keys.some((k) => k.day === d))
  const regions = REGIONS.filter((r) => keys.some((k) => k.region === r))
  if (days.length * regions.length === keys.length) return JSON.stringify({ day: days, region: regions })
  return JSON.stringify(keys.map((k) => ({ day: k.day, region: k.region })))
}

function counts(keys: Key[]) {
  const n = (code: string) => keys.filter((k) => k.code === code).length
  return { fresh: n('c'), pinned: n('p'), stale: n('s'), never_run: n('n'), failed: n('f'), blocked: n('b'), empty: 0 }
}

function overviewOf(states: string, error = ERROR) {
  const keys = keysOf(states)
  const failed = keys.filter((k) => k.code === 'f')
  const blocked = keys.filter((k) => k.code === 'b')
  const places = (of: Key[]) => [
    { dimension: 'day', words: DAYS.filter((d) => of.some((k) => k.day === d)).join(', ') },
    { dimension: 'region', words: REGIONS.filter((r) => of.some((k) => k.region === r)).join(', ') },
  ]
  return {
    id: NODE,
    chain: ['lake.py:raw', NODE],
    known: true,
    dimensions: [{ name: 'day', values: DAYS }, { name: 'region', values: REGIONS }],
    counts: counts(keys),
    needs_run: keys.filter((k) => NEEDS_RUN.includes(k.code)).length,
    errors: failed.length ? [{ error, keys: failed.length }] : [],
    issues: [
      ...(failed.length ? [{ state: 'failed', error: 0, title: error, keys: failed.length, places: places(failed), selection: selectionOf(failed) }] : []),
      ...(blocked.length ? [{ state: 'blocked', error: null, title: 'Waiting on the same keys of an input that failed', keys: blocked.length, places: places(blocked), selection: selectionOf(blocked) }] : []),
    ],
    along: 'day',
    rows: REGIONS.map((region) => ({ label: `region=${region}`, values: [region], states: keys.filter((k) => k.region === region).map((k) => k.code).join('') })),
    rows_total: REGIONS.length,
    truncated: false,
    last_attempt_at: '2026-09-21T12:00:00Z',
  }
}

/** What the keys route answers for `url`: the server's filters, applied to the table. */
function keysPageOf(states: string, url: URL, error = ERROR) {
  const q = url.searchParams
  const wanted = (q.get('state') ?? '').split(',').filter(Boolean)
  const values = new Map(q.getAll('value').map((v) => [v.slice(0, v.indexOf('=')), v.slice(v.indexOf('=') + 1).split(',')]))
  const all = keysOf(states)
  const matching = all.filter((k) => {
    const key = `day=${k.day},region=${k.region}`
    if (wanted.length && !wanted.includes(NAMES[k.code]!)) return false
    if (q.get('needs_run') === '1' && !NEEDS_RUN.includes(k.code)) return false
    if (q.has('error') && k.code !== 'f') return false
    if (q.get('q') && !key.includes(q.get('q')!.toLowerCase())) return false
    if (values.has('region') && !values.get('region')!.includes(k.region)) return false
    if (values.has('day') && !values.get('day')!.includes(k.day)) return false
    return true
  })
  const offset = Number(q.get('offset') ?? 0)
  const page = matching.slice(offset, offset + Number(q.get('limit') ?? 100))
  return {
    id: NODE,
    known: true,
    dimensions: ['day', 'region'],
    counts: counts(all),
    errors: all.some((k) => k.code === 'f') ? [{ error, keys: all.filter((k) => k.code === 'f').length }] : [],
    keys: page.map((k) => ({
      key: `day=${k.day},region=${k.region}`,
      values: [k.day, k.region],
      state: NAMES[k.code],
      attempted_at: k.code === 'b' ? null : '2026-09-21T12:00:00Z',
      elapsed_seconds: k.code === 'b' ? null : 0.005,
      size_bytes: k.code === 'c' ? 10 : null,
      rows: null,
      run_id: k.code === 'b' ? null : 'r1',
      error: k.code === 'f' ? error : null,
    })),
    total: matching.length,
    truncated: offset + page.length < matching.length,
    offset,
    needs_run: matching.filter((k) => NEEDS_RUN.includes(k.code)).length,
    selection: selectionOf(matching),
  }
}

const STATES = 'ccfcbc'
const detail = {
  id: NODE,
  key: 'day=d2,region=emea',
  state: 'f',
  needs_run: true,
  chain: [{ id: 'lake.py:raw', state: 'c' }, { id: NODE, state: 'f' }],
  attempts: [{ status: 'failed', created_at: '2026-09-21 12:00:00', elapsed_seconds: 0.007, error: 'ValueError: no rows for emea on d2\nTraceback', run_id: 'r1', artifact_path: null, format: null, size_bytes: null }],
  output: null,
  inputs: [{ id: 'lake.py:raw', step: 'lake.py:raw[day=d2,region=emea]', artifact_path: 'a/raw.json', format: 'json', size_bytes: 40, shape: { type: 'list', rows: 2, columns: [{ name: 'n', type: 'int' }], sample: [{ n: 0 }, { n: 1 }] } }],
}

/** Serve the explorer's routes from `states`; returns every keys query the page sent. */
async function mock(page: Page, states = STATES, error = ERROR): Promise<URL[]> {
  const asked: URL[] = []
  await page.route(/\/assets\/[^/]+\/partitions\/overview/, (route) => route.fulfill({ json: overviewOf(states, error) }))
  await page.route(/\/assets\/[^/]+\/partitions\/keys\?/, (route) => {
    const url = new URL(route.request().url())
    asked.push(url)
    return route.fulfill({ json: keysPageOf(states, url, error) })
  })
  await page.route(/\/assets\/[^/]+\/partitions\/key\?/, (route) => route.fulfill({ json: detail }))
  // The explorer no longer reads the whole grid: a request for it fails the test that made it.
  await page.route(/\/assets\/[^/]+\/partitions\/grid/, (route) => route.abort())
  return asked
}

test('says what is wrong and where, shows those keys, zooms and opens one', async ({ page }) => {
  const asked = await mock(page)
  await page.goto(`/ui/#/keys?node=${encodeURIComponent(NODE)}`)
  const bar = page.locator('.barca-view-head')
  await expect(bar.getByRole('heading', { name: 'events' })).toBeVisible()
  await expect(bar).toContainText('6 keys')

  // The counts, then each problem with where its keys are.
  const counts = page.getByRole('group', { name: 'Keys by state' })
  await expect(counts.getByRole('button', { name: '1 failed' })).toBeVisible()
  const issues = page.getByRole('region', { name: 'What needs attention' })
  await expect(issues).toContainText('ValueError: no rows for <region> on <day>')
  await expect(issues.getByRole('listitem').first()).toContainText('day d2')
  await expect(issues.getByRole('listitem').first()).toContainText('region emea')

  // The table starts with what needs attention: the failed and the blocked key, which the
  // server picks (`needs_run=1`).
  await expect(page.getByRole('table', { name: 'Keys' }).getByRole('row')).toHaveCount(3)
  expect(asked[0]!.searchParams.get('needs_run')).toBe('1')
  await page.getByRole('group', { name: 'Which keys' }).getByRole('button', { name: /^All/ }).click()
  await expect(page.getByRole('table', { name: 'Keys' }).getByRole('row')).toHaveCount(7)
  await expect(page).toHaveURL(/show=all/)
  expect(asked.at(-1)!.searchParams.has('needs_run')).toBe(false)

  // Show narrows the table to one problem's keys: the page asks the server for them; its Run computes them.
  await issues.getByRole('button', { name: 'Show' }).first().click()
  await expect(page).toHaveURL(/state=failed&error=0/)
  await expect(page.getByRole('table', { name: 'Keys' }).getByRole('row')).toHaveCount(2)
  expect(asked.at(-1)!.searchParams.get('state')).toBe('failed')
  expect(asked.at(-1)!.searchParams.get('error')).toBe('0')
  const started = page.waitForRequest((r) => r.method() === 'POST' && r.url().endsWith('/runs'))
  await page.route(/\/runs$/, (route) => route.request().method() === 'POST' ? route.fulfill({ json: { run_id: 'r9', handle: 'r9' } }) : route.fallback())
  await issues.getByRole('button', { name: 'Run' }).first().click()
  expect((await started).postDataJSON()).toMatchObject({ command: 'get', targets: [NODE], partition: JSON.stringify({ day: ['d2'], region: ['emea'] }) })
  await expect(page.locator('.barca-keys-notice')).toContainText('Started run r9')

  // The overview: one row per region; a column of one key opens it.
  const overview = page.getByRole('region', { name: 'Every key by state' })
  await expect(overview.getByRole('row')).toHaveCount(2)
  await overview.getByRole('row', { name: 'emea' }).locator('.barca-heat-bin').nth(1).dispatchEvent('mousedown')
  await overview.getByRole('row', { name: 'emea' }).locator('.barca-heat-bin').nth(1).dispatchEvent('mouseup')
  await expect(page).toHaveURL(/key=day%3Dd2%2Cregion%3Demea/)

  // The key: its error first, its chain, its input's first rows.
  const panel = page.getByRole('complementary', { name: 'Key day=d2,region=emea' })
  await expect(panel.getByRole('alert')).toContainText('ValueError: no rows for emea on d2')
  await expect(panel.getByRole('link', { name: 'raw' })).toHaveAttribute('href', /node=lake\.py%3Araw&key=/)
  await expect(panel.getByRole('table', { name: /raw for day=d2,region=emea/ })).toContainText('1')
  await page.keyboard.press('Escape')
  await expect(panel).toHaveCount(0)
})

test('the primary run follows search and state filters', async ({ page }) => {
  await mock(page)
  await page.goto(`/ui/#/keys?node=${encodeURIComponent(NODE)}&q=d2`)
  const run = page.locator('.barca-view-head').getByRole('button', { name: 'Run 1 key shown' })
  await expect(run).toBeVisible()
  await page.route(/\/runs$/, route => route.request().method() === 'POST' ? route.fulfill({ json: { run_id: 'r9' } }) : route.fallback())
  const request = page.waitForRequest(r => r.method() === 'POST' && r.url().endsWith('/runs'))
  await run.click()
  expect((await request).postDataJSON().partition).toBe(JSON.stringify({ day: ['d2'], region: ['emea'] }))
})

test('a failed key labels retained output as historical', async ({ page }) => {
  await mock(page)
  await page.route(/\/assets\/[^/]+\/partitions\/key\?/, route => route.fulfill({ json: { ...detail, output: detail.inputs[0] } }))
  await page.goto(`/ui/#/keys?node=${encodeURIComponent(NODE)}&key=day%3Dd2%2Cregion%3Demea`)
  const panel = page.getByRole('complementary', { name: 'Key day=d2,region=emea' })
  await expect(panel).toContainText('Last successful output')
  await expect(panel).toContainText('The latest attempt failed; this data is from an earlier successful attempt.')
})

test('retry completion refreshes the open key and reports failure', async ({ page }) => {
  await mock(page)
  let finished = false
  await page.route(/\/assets\/[^/]+\/partitions\/key\?/, route => route.fulfill({ json: {
    ...detail, attempts: finished ? [{ ...detail.attempts[0], error: 'ValueError: retry still failed', run_id: 'r9' }] : detail.attempts,
  } }))
  await page.route(/\/runs$/, route => route.request().method() === 'POST' ? route.fulfill({ json: { run_id: 'r9' } }) : route.fallback())
  await page.route(/\/runs\/r9$/, route => { finished = true; return route.fulfill({ json: { run_id: 'r9', status: 'failed' } }) })
  await page.goto(`/ui/#/keys?node=${encodeURIComponent(NODE)}&key=day%3Dd2%2Cregion%3Demea`)
  await page.getByRole('button', { name: 'Run this key' }).click()
  await expect(page.locator('.barca-keys-notice')).toContainText('Run failed')
  await expect(page.getByRole('complementary', { name: 'Key day=d2,region=emea' })).toContainText('ValueError: retry still failed')
})

test('repair names one key, records the reason, and refreshes its pin', async ({ page }) => {
  await mock(page)
  const step = `${NODE}[${detail.key}]`
  let pinned = false
  const pin = { node_id: step, run_hash: 'a'.repeat(64), pinned_at: '2026-11-08T12:00:00Z', pinned_by: null, reason: 'repair the broken feed', run_id: null, upload: null }
  await page.route(/\/assets\/[^/]+\/versions$/, route => route.fulfill({ json: {
    node_id: step, pinnable: true, pin: pinned ? pin : null, versions: [],
  } }))
  await page.route(/\/pins$/, route => route.fulfill({ json: { pins: pinned ? [pin] : [] } }))
  await page.route(/\/pin\/upload\?/, route => {
    pinned = true
    return route.fulfill({ json: { node_id: step, pin } })
  })
  await page.goto(`/ui/#/keys?node=${encodeURIComponent(NODE)}&key=${encodeURIComponent(detail.key)}`)
  const panel = page.getByRole('complementary', { name: `Key ${detail.key}` })
  await panel.getByLabel(`Replacement data for events[${detail.key}]`).setInputFiles({ name: 'repair.json', mimeType: 'application/json', buffer: Buffer.from('{"v":21}') })
  const dialog = page.getByRole('dialog')
  await expect(dialog).toContainText(`events[${detail.key}]`)
  await expect(dialog.getByLabel('Reason (optional)')).toBeFocused()
  await page.keyboard.press('Shift+Tab')
  await expect(dialog.getByRole('button', { name: 'Upload and pin' })).toBeFocused()
  await page.keyboard.press('Tab')
  await expect(dialog.getByLabel('Reason (optional)')).toBeFocused()
  await dialog.getByLabel('Reason (optional)').fill('repair the broken feed')
  const sent = page.waitForRequest(r => r.method() === 'POST' && r.url().includes('/pin/upload?'))
  await dialog.getByRole('button', { name: 'Upload and pin' }).click()
  const request = await sent
  const url = new URL(request.url())
  expect(decodeURIComponent(url.pathname)).toBe(`/assets/${step}/pin/upload`)
  expect(url.searchParams.get('reason')).toBe('repair the broken feed')
  expect(request.postData()).toBe('{"v":21}')
  await expect(panel).toContainText('Pinned at')
  await expect(panel).toContainText('repair the broken feed')
  await expect(page.getByRole('link', { name: '1 key pinned in events' })).toHaveAttribute('href', new RegExp(`keys\\?node=${encodeURIComponent(NODE)}&state=pinned$`))
})


test('both retry controls preserve scattered compound keys', async ({ page }) => {
  await mock(page, 'fccfcc', 'ValueError: bad feed')
  const requests: Record<string, unknown>[] = []
  await page.route(/\/runs$/, route => {
    if (route.request().method() !== 'POST') return route.fallback()
    requests.push(route.request().postDataJSON())
    return route.fulfill({ json: { run_id: 'retry-exact', handle: 'retry-exact' } })
  })
  await page.goto(`/ui/#/keys?node=${encodeURIComponent(NODE)}`)
  await page.getByRole('button', { name: 'Run 2 keys shown' }).click()
  await expect.poll(() => requests.length).toBe(1)
  await page.getByRole('region', { name: 'What needs attention' }).getByRole('button', { name: 'Run', exact: true }).click()
  await expect.poll(() => requests.length).toBe(2)
  for (const request of requests) {
    expect(JSON.parse(String(request.partition))).toEqual([
      { day: 'd1', region: 'emea' }, { day: 'd2', region: 'apac' },
    ])
  }
})

test('paused key retries ask before submitting the retained selection', async ({ page }) => {
  await mock(page)
  await page.route(/\/pauses$/, route => route.fulfill({ json: {
    total: 1, paused: [{ id: NODE, paused_at: 1, via: NODE }],
  } }))
  const requests: Record<string, unknown>[] = []
  await page.route(/\/runs$/, route => {
    if (route.request().method() !== 'POST') return route.fallback()
    requests.push(route.request().postDataJSON())
    return route.fulfill({ json: { run_id: 'paused-retry', handle: 'paused-retry' } })
  })
  await page.goto(`/ui/#/keys?node=${encodeURIComponent(NODE)}`)
  await page.getByRole('button', { name: 'Run 2 keys shown' }).click()
  const dialog = page.getByRole('dialog')
  await expect(dialog).toContainText('only the selected keys')
  expect(requests).toHaveLength(0)
  await dialog.getByRole('button', { name: 'Cancel' }).click()
  expect(requests).toHaveLength(0)
  await page.getByRole('button', { name: 'Run 2 keys shown' }).click()
  await dialog.getByRole('button', { name: 'Run anyway' }).click()
  await expect.poll(() => requests.length).toBe(1)
  expect(requests[0]).toMatchObject({ allow_paused: true, targets: [NODE] })
  expect(JSON.parse(String(requests[0]!.partition))).toEqual({ day: ['d2', 'd3'], region: ['emea'] })
})

test('read-only key inspection does not offer execution', async ({ page }) => {
  await mock(page)
  await page.route(/\/health$/, route => route.fulfill({ json: { read_only: true } }))
  await page.goto(`/ui/#/keys?node=${encodeURIComponent(NODE)}&key=day%3Dd2%2Cregion%3Demea`)
  // A read-only server offers no run at all, and says once that it is read-only.
  await expect(page.getByRole('note').filter({ hasText: 'Read-only.' })).toBeVisible()
  await expect(page.getByRole('button', { name: 'Run 2 keys shown' })).toHaveCount(0)
  await expect(page.getByRole('region', { name: 'What needs attention' }).getByRole('button', { name: 'Run', exact: true })).toHaveCount(0)
  await expect(page.getByRole('button', { name: 'Run this key' })).toHaveCount(0)
})

test('each filter and each order asks the server again, with the route\'s own parameters', async ({ page }) => {
  const asked = await mock(page)
  await page.goto(`/ui/#/keys?node=${encodeURIComponent(NODE)}&show=all`)
  const table = page.getByRole('table', { name: 'Keys' })
  await expect(table.getByRole('row')).toHaveCount(7)
  const last = () => asked.at(-1)!.searchParams

  // A dimension's value.
  await page.getByLabel('region').selectOption('apac')
  await expect(table.getByRole('row')).toHaveCount(4)
  expect(last().getAll('value')).toEqual(['region=apac'])
  await expect(page.locator('.barca-keys-table')).toContainText('3 keys shown')

  // Free text over the key.
  await page.getByPlaceholder('Search keys').fill('d3')
  await expect(table.getByRole('row')).toHaveCount(2)
  expect(last().get('q')).toBe('d3')
  expect(last().getAll('value')).toEqual(['region=apac'])
  await page.getByRole('button', { name: 'Clear' }).click()
  await expect(table.getByRole('row')).toHaveCount(7)

  // An order: the header asks for it, and pressing it again returns to key order.
  await table.getByRole('button', { name: 'Last attempt' }).click()
  await expect(page).toHaveURL(/sort=attempted/)
  await expect.poll(() => last().get('sort')).toBe('attempted')
  await expect(table.getByRole('columnheader', { name: /Last attempt/ })).toHaveAttribute('aria-sort', 'descending')
  await table.getByRole('button', { name: /Last attempt/ }).click()
  await expect(page).not.toHaveURL(/sort=/)

  // A stretch of the long dimension, named as the route takes it.
  await page.goto(`/ui/#/keys?node=${encodeURIComponent(NODE)}&show=all&range=d2..d3`)
  await expect.poll(() => last().get('range')).toBe('day=d2..d3')
})

test('a long list is read a page at a time as it is scrolled', async ({ page }) => {
  // 500 keys match; the server lists the page asked for and says more follow.
  const offsets: number[] = []
  await page.route(/\/assets\/[^/]+\/partitions\/overview/, (route) => route.fulfill({ json: { ...overviewOf(STATES), counts: { fresh: 500, pinned: 0, stale: 0, never_run: 0, failed: 0, blocked: 0, empty: 0 }, needs_run: 0, issues: [] } }))
  await page.route(/\/assets\/[^/]+\/partitions\/keys\?/, (route) => {
    const url = new URL(route.request().url())
    const offset = Number(url.searchParams.get('offset'))
    const limit = Number(url.searchParams.get('limit'))
    offsets.push(offset)
    const keys = Array.from({ length: Math.min(limit, 500 - offset) }, (_, i) => ({
      key: `day=k${offset + i},region=emea`, values: [`k${offset + i}`, 'emea'], state: 'fresh', attempted_at: null, elapsed_seconds: null, size_bytes: null, rows: null, run_id: null, error: null,
    }))
    return route.fulfill({ json: { ...keysPageOf(STATES, url), keys, total: 500, truncated: offset + keys.length < 500, offset, needs_run: 0, selection: null } })
  })
  await page.goto(`/ui/#/keys?node=${encodeURIComponent(NODE)}`)
  const table = page.getByRole('table', { name: 'Keys' })
  await expect(table).toHaveAttribute('aria-rowcount', '501')
  await expect(table.getByRole('row').nth(1)).toContainText('k0')
  expect(offsets).toEqual([0])
  // Only the first page is in the browser; scrolling to its end asks for the next.
  await page.locator('.barca-keytable-body').evaluate((el) => el.scrollTo(0, el.scrollHeight))
  await expect.poll(() => offsets).toContain(200)
  await expect(page.getByRole('button', { name: 'All fresh' })).toBeDisabled()
})
