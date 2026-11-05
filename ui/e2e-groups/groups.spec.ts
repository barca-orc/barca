import { expect, test } from '@playwright/test'
import { isStartRun, seedRuns, waitForRun } from '../e2e/helpers/seed'

// The grouped page against a project declared in groups (fixture/grouped.py), served for real:
// the groups come from `GET /groups`, the states from `GET /state`, the failure from a run of
// the check that fails. The eight state words are the CLI's; a failed last attempt is a fact
// beside the state, never in its place.

const FILE = 'grouped.py'
const G = (name: string) => `group:${FILE}:${name}`
const N = (name: string) => `${FILE}:${name}`
const node = (id: string) => `.react-flow__node[data-id="${id}"]`
const CACHE_STATES = ['running', 'blocked', 'stale', 'partial', 'unsynced', 'unknown', 'never_run', 'always_runs', 'cached', 'last attempt failed']

// The whole pipeline once, and the fold 2 check (which fails) once: a failed last attempt on
// record beside cached models.
test.beforeAll(async ({ request }) => {
  await seedRuns(request, N('publish'), 1)
  await seedRuns(request, N('check__fold_02__model'), 1)
})

test('double-click navigates nested groups and List keeps the current scope after reload', async ({ page }) => {
  await page.goto(`/ui/#/assets?pipeline=${FILE}&view=graph`)
  // The two root groups and the ungrouped task.
  await expect(page.locator('.react-flow__node')).toHaveCount(3)
  await page.locator(node(G('cross_validation'))).dblclick()
  await expect(page.locator('.react-flow__node')).toHaveCount(4) // two folds, the summary, a port for clean_rows
  await page.locator(node(G('fold_02'))).dblclick()
  await expect(page.locator('.react-flow__node')).toHaveCount(3) // the model, its check, a port
  const crumbs = page.getByRole('navigation', { name: 'Group breadcrumb' })
  await expect(crumbs).toContainText('Cross-validation')
  await expect(crumbs).toContainText('Fold 2')
  await page.getByRole('button', { name: 'List', exact: true }).click()
  await expect(page.locator('.barca-group-table tbody tr')).toHaveCount(2)
  await page.reload()
  await expect(page.getByRole('heading', { name: 'Fold 2', exact: true })).toBeVisible()
  await expect(page.locator('.barca-group-table tbody tr')).toHaveCount(2)
  await page.getByRole('button', { name: 'Up one level' }).click()
  await expect(page.locator('.barca-group-table tbody tr')).toHaveCount(3)
})

test('a failed check is a fact beside its ancestors and itself; the model stays cached; navigation sends no run requests', async ({ page }) => {
  const mutations: string[] = []
  page.on('request', (request) => {
    if (request.method() !== 'GET') mutations.push(request.url())
  })
  await page.goto(`/ui/#/assets?pipeline=${FILE}&view=graph`)
  const cv = page.locator(node(G('cross_validation')))
  await expect(cv.locator('.barca-group-node[data-tone="failed"]')).toBeVisible()
  await expect(cv).toContainText('1 last attempt failed')
  await cv.click()
  const panel = page.getByRole('complementary', { name: 'Cross-validation group details' })
  await expect(panel).toContainText('1 last attempt failed')
  // The output's own state, in the CLI's word.
  await expect(panel.getByText('cached', { exact: true })).toBeVisible()
  await expect(panel).toContainText('check__fold_02__model')
  await panel.getByRole('button', { name: /check__fold_02__model/ }).click()
  await expect(page.getByRole('navigation', { name: 'Group breadcrumb' })).toContainText('Fold 2')
  const check = page.locator(node(N('check__fold_02__model')))
  await expect(check.locator('.barca-dag[data-failed]')).toBeVisible()
  await expect(check).toContainText('always_runs')
  await expect(check).toContainText('last attempt failed')
  const model = page.locator(node(N('fold_02__model')))
  await expect(model.locator('.barca-dag[data-tone="success"]')).toBeVisible()
  await expect(model).toContainText('cached')
  await expect(model.locator('.barca-dag[data-failed]')).toHaveCount(0)
  // The node panel opens for the failed check with its error, from the real server.
  await expect(page.getByRole('complementary', { name: 'check__fold_02__model details' })).toContainText('feature count mismatch')
  expect(mutations).toEqual([])
})

test('the legend names every state in the CLI\'s words', async ({ page }) => {
  await page.goto(`/ui/#/assets?pipeline=${FILE}&view=graph`)
  const legend = page.locator('.barca-legend')
  for (const word of CACHE_STATES) await expect(legend).toContainText(word)
  await expect(legend).not.toContainText(/Healthy|Not run|Stale/)
})

test('table groups support search, arrow selection and Enter to open', async ({ page }) => {
  await page.goto(`/ui/#/assets?pipeline=${FILE}`)
  const rows = page.locator('.barca-group-table tbody tr')
  await expect(rows).toHaveCount(3)
  // Worst first: the group with the failed check heads the table.
  await expect(rows.first()).toContainText('Cross-validation')
  await rows.first().click()
  await expect(rows.first()).toHaveAttribute('aria-selected', 'true')
  await page.keyboard.press('ArrowDown')
  await expect(rows.nth(1)).toHaveAttribute('aria-selected', 'true')
  await page.keyboard.press('ArrowUp')
  await expect(rows.first()).toHaveAttribute('aria-selected', 'true')
  await page.keyboard.press('Enter')
  await expect(page.getByRole('heading', { name: 'Cross-validation', exact: true })).toBeVisible()
  await expect(rows).toHaveCount(3)
  await page.getByRole('navigation', { name: 'Group breadcrumb' }).getByRole('button', { name: FILE, exact: true }).click()
  await expect(page).not.toHaveURL(/group=/)
  await expect(rows).toHaveCount(3)
  await page.getByPlaceholder('Search groups and members').fill('fold_02__model')
  // The group that holds the matches, then every matching node in it, with its path.
  await expect(rows).toHaveCount(3)
  await expect(rows.first()).toContainText('Cross-validation')
  await expect(rows.nth(1)).toContainText('in Cross-validation › Fold 2')
  await rows.nth(1).click()
  await expect(page.getByRole('navigation', { name: 'Group breadcrumb' })).toContainText('Fold 2')
})

test('All nodes shows every node flat and Groups restores the cards', async ({ page }) => {
  await page.goto(`/ui/#/assets?pipeline=${FILE}&view=graph`)
  await expect(page.locator('.react-flow__node')).toHaveCount(3)
  await page.getByRole('button', { name: 'All nodes', exact: true }).click()
  await expect(page.locator('.react-flow__node')).toHaveCount(9)
  await page.getByRole('button', { name: 'Groups', exact: true }).click()
  await expect(page.locator('.react-flow__node')).toHaveCount(3)
})

test('groups open worst first, a sort is kept in the URL, and rows read by their distinct part', async ({ page }) => {
  await page.goto(`/ui/#/assets?pipeline=${FILE}`)
  const rows = page.locator('.barca-group-table tbody tr')
  await expect(rows.first()).toContainText('Cross-validation')
  await expect(rows.first()).toContainText('1 last attempt failed')
  await expect(page.getByRole('columnheader', { name: 'Failed' })).toBeVisible()
  await expect(page.getByRole('columnheader', { name: 'Output' })).toHaveCount(0)
  await page.getByLabel('Sort').selectOption('name')
  await expect(page).toHaveURL(/sort=name/)
  await expect(rows.first()).toContainText('Cross-validation')
  await page.goto(`/ui/#/assets?pipeline=${FILE}&group=${G('source')}`)
  await expect(page.getByRole('columnheader', { name: 'Duration' })).toBeVisible()
  await expect(page.getByRole('columnheader', { name: 'Contents' })).toHaveCount(0)
  // A row's state is the CLI's word; the reason the server gives is beside it.
  const check = page.getByRole('row', { name: /check__clean_rows/ })
  await expect(check).toContainText('always_runs')
  await expect(check.locator('.barca-dag[data-failed], .barca-tag[data-tone="failed"]')).toHaveCount(0)
})

test('failures are grouped by cause, and a failure row retries its node and opens its run', async ({ page }) => {
  await page.goto(`/ui/#/assets?pipeline=${FILE}&show=failures`)
  await expect(page.locator('.barca-failure-cause')).toHaveCount(1)
  await expect(page.locator('.barca-failure-cause')).toContainText('1 failed')
  await expect(page.locator('.barca-failure-cause')).toContainText('feature count mismatch')
  await expect(page.getByRole('button', { name: /^Mute / })).toHaveCount(0)
  const started = page.waitForResponse((response) => isStartRun(response.request(), 'run', N('check__fold_02__model')))
  await page.getByRole('button', { name: /^Retry / }).click()
  const response = await started
  expect(response.status()).toBe(202)
  const handle = (await response.json()).run_id
  // The shared run line follows the retry.
  await expect(page.getByRole('status').filter({ hasText: /check__fold_02__model/ })).toBeVisible()
  await waitForRun(page.request, handle)
  await page.getByRole('button', { name: /^Open run of / }).click()
  await expect(page).toHaveURL(/#\/runs\?run=/)
  await expect(page.getByRole('complementary', { name: 'Run details' })).toContainText('feature count mismatch')
})

test('All pipelines is a map of files, and a pipeline of one group opens that group', async ({ page }) => {
  await page.goto('/ui/#/assets?view=graph')
  await expect(page.locator('.react-flow__node')).toHaveCount(3)
  await expect(page.getByText('32 nodes · 3 pipelines')).toBeVisible()
  // The one edge between files carries how many inputs cross it.
  await expect(page.locator('.react-flow__edge-text')).toHaveText('22')
  await page.locator(`.react-flow__node[data-id="domain:${FILE}"]`).dblclick()
  await expect(page).toHaveURL(/pipeline=grouped\.py/)
  await expect(page.getByRole('navigation', { name: 'Group breadcrumb' })).toHaveText(FILE)
  await expect(page.getByText('9 nodes · 3 groups')).toBeVisible()
  // marts.py is one group: it opens at once, its 22 marts in a grid fed by one port.
  await page.goto('/ui/#/assets?view=graph')
  await page.locator('.react-flow__node[data-id="domain:marts.py"]').dblclick()
  await expect(page.getByRole('navigation', { name: 'Group breadcrumb' })).toHaveText('marts.py')
  await expect(page.getByText('22 nodes · 22 items')).toBeVisible()
  await expect(page.locator('.barca-port-node')).toHaveText('summary')
  const rows = await page.locator('.react-flow__node-asset').evaluateAll((all) => new Set(all.map((n) => n.getBoundingClientRect().top)).size)
  expect(rows).toBeLessThan(8)
  // A fold's inputs from outside it are one faded port too.
  await page.goto(`/ui/#/assets?pipeline=${FILE}&view=graph&group=${G('fold_01')}`)
  await expect(page.locator('.barca-port-node')).toHaveText('clean_rows')
})

test('the grouped page header fits a 1280×720 window on one line', async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 720 })
  await page.goto(`/ui/#/assets?pipeline=${FILE}&group=${G('fold_02')}`)
  await expect(page.getByRole('heading', { name: 'Fold 2', exact: true })).toBeVisible()
  await expect(page.locator('.barca-group-external')).toHaveText('External inputs · 1')
  const bar = await page.locator('.barca-view-bar').evaluate((el) => {
    const boxes = [...el.children].map((child) => child.getBoundingClientRect())
    const heading = el.querySelector('h1')!
    return {
      height: el.getBoundingClientRect().height,
      tallest: Math.max(...boxes.map((box) => box.height)),
      overlaps: boxes.slice(1).filter((box, i) => box.left < boxes[i]!.right).length,
      headingCut: heading.scrollWidth > heading.clientWidth,
      parts: [...el.children].map((child) => `${child.className} ${Math.round(child.getBoundingClientRect().width)}px`).join(' | '),
    }
  })
  expect(bar.height, `one line: ${bar.parts}`).toBeLessThanOrEqual(36)
  expect(bar.tallest, `no child wrapped: ${bar.parts}`).toBeLessThanOrEqual(36)
  expect(bar.overlaps, `no overlap: ${bar.parts}`).toBe(0)
  expect(bar.headingCut, `the heading is whole: ${bar.parts}`).toBe(false)
})

for (const view of ['list', 'graph']) {
  for (const [target, verb] of [['summary', 'get'], ['publish', 'run']] as const) {
    test(`${view}: runs the grouped ${target} with ${verb}`, async ({ page, request }) => {
      const nodeId = N(target)
      const group = target === 'summary' ? G('cross_validation') : null
      const selection = view === 'graph' ? `view=graph&focus=${nodeId}` : `node=${nodeId}`
      await page.goto(`/ui/#/assets?pipeline=${FILE}${group ? `&group=${group}` : ''}&${selection}`)
      if (group) await expect(page.getByRole('navigation', { name: 'Group breadcrumb' })).toContainText('Cross-validation')
      const panel = page.getByRole('complementary', { name: `${target} details` })
      const topbar = page.getByRole('banner').getByRole('button', { name: `Run ${target}`, exact: true })
      await expect(topbar).toBeEnabled()
      await expect(panel.getByRole('button', { name: 'Run', exact: true })).toBeEnabled()
      const started = page.waitForResponse((response) => isStartRun(response.request(), verb, nodeId))
      await (view === 'graph' ? topbar : panel.getByRole('button', { name: 'Run', exact: true })).click()
      const response = await started
      expect(response.status()).toBe(202)
      const handle = (await response.json()).run_id
      await page.getByRole('link', { name: 'View run', exact: true }).click()
      await expect(page).toHaveURL(new RegExp(`#/runs\\?run=${handle}`))
      const details = page.getByRole('complementary', { name: 'Run details' })
      await expect(details).toContainText(target)
      await waitForRun(request, handle)
      await expect(details).toContainText('success', { timeout: 30_000 })
      if (verb === 'run') await expect(details).toContainText('published 2 folds')
    })
  }

  test(`${view}: read-only mode disables both run actions`, async ({ page }) => {
    await page.route('**/health', async (route) => {
      const response = await route.fetch()
      await route.fulfill({ json: { ...(await response.json()), read_only: true } })
    })
    const selection = view === 'graph' ? `view=graph&focus=${N('raw_rows')}` : `node=${N('raw_rows')}`
    await page.goto(`/ui/#/assets?pipeline=${FILE}&group=${G('source')}&${selection}`)
    const topbar = page.getByRole('banner').getByRole('button', { name: 'Run raw_rows', exact: true })
    const panel = page.getByRole('complementary', { name: 'raw_rows details' })
    await expect(topbar).toBeDisabled()
    await expect(topbar).toHaveAttribute('title', 'This server is read-only')
    await expect(panel.getByRole('button', { name: 'Run', exact: true })).toBeDisabled()
    await expect(panel.getByRole('button', { name: 'Run', exact: true })).toHaveAttribute('title', 'This server is read-only')
  })
}

test('the grouped list topbar shows running before the first stream event arrives', async ({ page }) => {
  let release!: () => void
  const held = new Promise<void>((resolve) => { release = resolve })
  await page.route(/\/runs\/[^/]+\/events$/, async (route) => {
    await held
    await route.continue()
  })
  try {
    await page.goto(`/ui/#/assets?pipeline=${FILE}&group=${G('cross_validation')}&node=${N('summary')}`)
    const started = page.waitForResponse((response) => isStartRun(response.request(), 'get', N('summary')))
    await page.getByRole('banner').getByRole('button', { name: 'Run summary', exact: true }).click()
    expect((await started).status()).toBe(202)
    const row = page.getByRole('row').filter({ hasText: 'summary' })
    await expect(row).toContainText('running')
    await expect(page.getByRole('complementary', { name: 'summary details' })).toContainText('running')
    // The breadcrumb's root is the open pipeline.
    await page.getByRole('navigation', { name: 'Group breadcrumb' }).getByRole('button', { name: FILE, exact: true }).click()
    await expect(page.locator(`tr[data-group="${G('cross_validation')}"]`)).toContainText('1 running')
  } finally {
    release()
  }
})

test('a refused grouped run shows its error in the node panel', async ({ page }) => {
  await page.route('**/runs', (route) => route.request().method() === 'POST'
    ? route.fulfill({ status: 503, json: { error: 'Run queue is unavailable' } })
    : route.continue())
  await page.goto(`/ui/#/assets?pipeline=${FILE}&group=${G('source')}&node=${N('raw_rows')}`)
  const panel = page.getByRole('complementary', { name: 'raw_rows details' })
  await panel.getByRole('button', { name: 'Run', exact: true }).click()
  await expect(panel.getByRole('alert')).toContainText('Run queue is unavailable')
})

test('a status failure leaves grouped metadata visible', async ({ page }) => {
  await page.route(/\/state(?:\?.*)?$/, (route) => route.fulfill({
    status: 500, json: { error: 'History is unavailable', remediation: 'Check the metadata volume' },
  }))
  await page.goto(`/ui/#/assets?pipeline=${FILE}`)
  await expect(page.locator('.barca-group-table tbody tr')).toHaveCount(3)
  await expect(page.getByRole('status')).toContainText('History is unavailable')
  await expect(page.getByRole('status')).toContainText('Check the metadata volume')
})
