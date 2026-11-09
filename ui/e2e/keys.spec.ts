import { expect, test, type Page } from '@playwright/test'

// The key explorer against fixed answers: a day × region grid of six keys, one failed and one
// blocked behind an input, so every control has something to act on.
const NODE = 'lake.py:events'
const grid = {
  id: NODE,
  chain: ['lake.py:raw', NODE],
  known: true,
  dimensions: [
    { name: 'day', values: ['d1', 'd2', 'd3'] },
    { name: 'region', values: ['emea', 'apac'] },
  ],
  states: 'ccfcbc',
  counts: { fresh: 4, stale: 0, never_run: 0, failed: 1, blocked: 1 },
  attempted_base: 1_790_000_000,
  attempted_at: [0, 0, 10, 0, null, 0],
  elapsed_ms: [5, 6, 7, 5, null, 5],
  size_bytes: [10, 10, null, 10, null, 10],
  run: [0, 0, 0, 0, null, 0],
  error: { '2': 0 },
  errors: [{ error: 'ValueError: no rows for <region> on <day>', keys: 1 }],
  runs: ['r1'],
}
const detail = {
  id: NODE,
  key: 'day=d2,region=emea',
  state: 'f',
  chain: [{ id: 'lake.py:raw', state: 'c' }, { id: NODE, state: 'f' }],
  attempts: [{ status: 'failed', created_at: '2026-09-21 12:00:00', elapsed_seconds: 0.007, error: 'ValueError: no rows for emea on d2\nTraceback', run_id: 'r1', artifact_path: null, format: null, size_bytes: null }],
  output: null,
  inputs: [{ id: 'lake.py:raw', step: 'lake.py:raw[day=d2,region=emea]', artifact_path: 'a/raw.json', format: 'json', size_bytes: 40, shape: { type: 'list', rows: 2, columns: [{ name: 'n', type: 'int' }], sample: [{ n: 0 }, { n: 1 }] } }],
}

async function mock(page: Page) {
  await page.route(/\/assets\/[^/]+\/partitions\/grid/, (route) => route.fulfill({ json: grid }))
  await page.route(/\/assets\/[^/]+\/partitions\/key\?/, (route) => route.fulfill({ json: detail }))
}

test('says what is wrong and where, shows those keys, zooms and opens one', async ({ page }) => {
  await mock(page)
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

  // The table starts with what needs attention: the failed and the blocked key.
  await expect(page.getByRole('table', { name: 'Keys' }).getByRole('row')).toHaveCount(3)
  await page.getByRole('group', { name: 'Which keys' }).getByRole('button', { name: /^All/ }).click()
  await expect(page.getByRole('table', { name: 'Keys' }).getByRole('row')).toHaveCount(7)
  await expect(page).toHaveURL(/show=all/)

  // Show narrows the table to one problem's keys; its Run computes them.
  await issues.getByRole('button', { name: 'Show' }).first().click()
  await expect(page).toHaveURL(/state=failed&error=0/)
  await expect(page.getByRole('table', { name: 'Keys' }).getByRole('row')).toHaveCount(2)
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
