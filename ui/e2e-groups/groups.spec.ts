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
// A node at rest and each part of a group card read as the health map reads them (#1036): the
// served cause in its word, or `fresh`, with a dot in the map's colour; run work and a pin keep
// their own glyph and word. The legend names what is on screen.
const MAP_WORDS = ['failing', 'overdue', 'no refresh path', 'queued', 'undecided', 'catching up', 'fresh']
const OTHER_WORDS = ['running', 'pinned']

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
  await expect(cv).toContainText('1 failing')
  await cv.click()
  const panel = page.getByRole('complementary', { name: 'Cross-validation group details' })
  await expect(panel).toContainText('1 failing')
  // The output's own state, in the CLI's word.
  await expect(panel.getByText('fresh', { exact: true })).toBeVisible()
  await expect(panel).toContainText('check__fold_02__model')
  await panel.getByRole('button', { name: /check__fold_02__model/ }).click()
  await expect(page.getByRole('navigation', { name: 'Group breadcrumb' })).toContainText('Fold 2')
  const check = page.locator(node(N('check__fold_02__model')))
  await expect(check.locator('.barca-dag[data-failed]')).toBeVisible()
  await expect(check).toContainText('failing')
  const model = page.locator(node(N('fold_02__model')))
  await expect(model.locator('.barca-dag[data-tone="success"]')).toBeVisible()
  await expect(model).toContainText('fresh')
  await expect(model.locator('.barca-dag[data-failed]')).toHaveCount(0)
  // The node panel opens for the failed check with its error, from the real server.
  await expect(page.getByRole('complementary', { name: 'check__fold_02__model details' })).toContainText('feature count mismatch')
  expect(mutations).toEqual([])
})

test('the legend names the states the graph shows, in the health map\'s words and colours', async ({ page }) => {
  await page.goto(`/ui/#/assets?pipeline=${FILE}&view=graph`)
  const legend = page.locator('.barca-legend')
  await expect(legend).toContainText('failing')
  const words = (await legend.locator('span').allTextContents()).map((w) => w.trim())
  for (const word of words) expect([...MAP_WORDS, ...OTHER_WORDS, 'Group', 'Pipeline']).toContain(word)
  // Each cause is a dot in the colour the map gives it; no cache-state or older health word is left.
  await expect(legend.locator('.barca-cause-dot[data-cause="failing"]')).toHaveCount(1)
  await expect(legend).not.toContainText(/Healthy|Not run|Stale|blocked|behind|never run|succeeded|\bok\b/)
  // A group card leads with its cause in words, and marks every part with the same dots.
  const cv = page.locator(node(G('cross_validation')))
  await expect(cv.locator('.barca-group-node-lead .barca-cause-dot')).toHaveAttribute('data-cause', 'failing')
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
  // Every matching node, with its path; not the group that holds them, which does not match.
  await expect(rows).toHaveCount(2)
  await expect(rows.first()).toContainText('in Cross-validation › Fold 2')
  await expect(rows.nth(1)).toContainText('in Cross-validation › Fold 2')
  await rows.first().click()
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
  await expect(rows.first()).toContainText('1 failing')
  await expect(page.getByRole('columnheader', { name: 'Failed' })).toBeVisible()
  await expect(page.getByRole('columnheader', { name: 'Output' })).toHaveCount(0)
  await page.getByLabel('Sort').selectOption('name')
  await expect(page).toHaveURL(/sort=name/)
  await expect(rows.first()).toContainText('Cross-validation')
  await page.goto(`/ui/#/assets?pipeline=${FILE}&group=${G('source')}`)
  await expect(page.getByRole('columnheader', { name: 'Duration' })).toBeVisible()
  await expect(page.getByRole('columnheader', { name: 'Contents' })).toHaveCount(0)
  // A row's state is its cause, in the health map's word, and when it will be fresh or why
  // there is no estimate. A check (a task) that never ran and that nothing scheduled runs:
  const check = page.getByRole('row', { name: /check__clean_rows/ })
  await expect(check.locator('.barca-cause')).toHaveAttribute('data-cause', 'no_refresh_path')
  await expect(check).toContainText('no refresh path')
  await expect(check).toContainText('no estimate: no refresh path')
  await expect(page.getByRole('columnheader', { name: 'Fresh by' })).toBeVisible()
  await expect(page.getByRole('columnheader', { name: 'Last success' })).toHaveCount(0)
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

test('All pipelines draws what its list lists: the groups and loose nodes, with the edges between them', async ({ page }) => {
  // Source data, Cross-validation and Marts; publish and other.py's standalone, which no group holds.
  const top = [G('source'), G('cross_validation'), 'group:marts.py:marts', N('publish'), 'other.py:standalone']
  await page.goto('/ui/#/assets')
  await expect(page.locator('.barca-group-table tbody tr')).toHaveCount(top.length)
  await page.goto('/ui/#/assets?view=graph')
  await expect(page.locator('.react-flow__node')).toHaveCount(top.length)
  await expect(page.getByText('28 assets · 4 tasks in 5 groups')).toBeVisible()
  for (const id of top) await expect(page.locator(node(id))).toHaveCount(1)
  // Edges by dependency, never a grid without them: source feeds the folds, which feed the marts and publish.
  for (const [source, target] of [[G('source'), G('cross_validation')], [G('cross_validation'), 'group:marts.py:marts'], [G('cross_validation'), N('publish')]]) {
    await expect(page.locator(`.react-flow__edge[data-id="${source}->${target}"]`)).toHaveCount(1)
  }
  const [x0, x1] = await Promise.all([G('source'), G('cross_validation')].map(async (id) => (await page.locator(node(id)).boundingBox())!.x))
  expect(x1).toBeGreaterThan(x0!)
  // A failing group says so first, and the legend lists it.
  await expect(page.locator(node(G('cross_validation')))).toContainText('1 failing')
  await expect(page.locator('.barca-legend')).toContainText('failing')
  await page.locator(node('group:marts.py:marts')).dblclick()
  await expect(page.getByRole('navigation', { name: 'Group breadcrumb' })).toContainText('Marts')
  // marts.py is one group: it opens at once, its 22 marts in a grid fed by one port.
  await page.goto('/ui/#/assets?pipeline=marts.py&view=graph')
  await expect(page.getByRole('navigation', { name: 'Group breadcrumb' })).toHaveText('marts.py')
  await expect(page.getByText('22 assets in 1 group')).toBeVisible()
  await expect(page.locator('.barca-port-node')).toHaveText('summary')
  const rows = await page.locator('.react-flow__node-asset').evaluateAll((all) => new Set(all.map((n) => n.getBoundingClientRect().top)).size)
  expect(rows).toBeLessThan(8)
  // A fold's inputs from outside it are one faded port too.
  await page.goto(`/ui/#/assets?pipeline=${FILE}&view=graph&group=${G('fold_01')}`)
  await expect(page.locator('.barca-port-node')).toHaveText('clean_rows')
})

test('a group\'s graph keeps its labels at 11px or more, and moving between levels never flashes Checking…', async ({ page }) => {
  // A group's own state answers late, as it does on a busy server.
  await page.route(/\/state\?.*targets=/, async (route) => {
    await new Promise((resolve) => setTimeout(resolve, 600))
    await route.continue()
  })
  await page.goto(`/ui/#/assets?pipeline=${FILE}&view=graph`)
  await expect(page.locator(node(G('cross_validation')))).toContainText('1 failing')
  const pending = page.locator('.react-flow__node [data-glyph="pending"]')
  for (const group of [G('cross_validation'), G('fold_02'), G('source')]) {
    await page.evaluate((g) => { location.hash = `/assets?pipeline=grouped.py&view=graph&group=${encodeURIComponent(g)}` }, group)
    // Sampled while the open group's own state request is in flight.
    for (let i = 0; i < 6; i++) {
      expect(await pending.count()).toBe(0)
      await page.waitForTimeout(40)
    }
    await page.waitForTimeout(500)
    const smallest = await page.evaluate(() => {
      const zoom = new DOMMatrix(getComputedStyle(document.querySelector('.react-flow__viewport')!).transform).a
      const sizes = [...document.querySelectorAll('.react-flow__node *')]
        .filter((el) => [...el.childNodes].some((c) => c.nodeType === Node.TEXT_NODE && c.textContent!.trim()) && !el.closest('.barca-sr-only'))
        .map((el) => parseFloat(getComputedStyle(el).fontSize))
      return Math.min(...sizes) * zoom
    })
    expect(smallest).toBeGreaterThanOrEqual(11 - 1e-6)
  }
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

// A deep path with long names at 1440×900 (#970): the header used to cut the breadcrumb, the
// contents line and the health summary all at once. Now only the crumbs between the root and the
// heading are shortened (whole in their tooltip), and what does not fit wraps to a second line.
test('the grouped page header is readable at 1440×900 under a deep path with long names', async ({ page }) => {
  const names: Record<string, string> = {
    [G('cross_validation')]: 'Cross-validation of every candidate planning model',
    [G('fold_02')]: 'Second held-out fold of the driver assumptions',
  }
  await page.route('**/groups', async (route) => {
    const groups = await (await route.fetch()).json() as { id: string; name: string }[]
    await route.fulfill({ json: groups.map((g) => ({ ...g, name: names[g.id] ?? g.name })) })
  })
  await page.setViewportSize({ width: 1440, height: 900 })
  await page.goto(`/ui/#/assets?pipeline=${FILE}&group=${G('fold_02')}`)
  await expect(page.getByRole('heading', { name: names[G('fold_02')]!, exact: true })).toBeVisible()
  await expect(page.locator('.barca-scope-health .barca-health-label')).toContainText('failing')
  const bar = await page.locator('.barca-view-bar').evaluate((el) => {
    const cut = (sel: string) => [...el.querySelectorAll(sel)].filter((e) => e.scrollWidth > e.clientWidth).map((e) => e.textContent)
    const right = el.getBoundingClientRect().right
    return {
      height: el.getBoundingClientRect().height,
      cut: cut('h1, .barca-group-crumbs > button, :scope > .barca-count, .barca-health-label, .barca-group-external'),
      middle: [...el.querySelectorAll('.barca-group-crumbs > span > button')].map((e) => ({ title: e.getAttribute('title'), width: e.getBoundingClientRect().width })),
      overflowing: [...el.children].filter((child) => child.getBoundingClientRect().right > right + 0.5).map((child) => child.className),
    }
  })
  expect(bar.cut, 'the heading, the root, the contents and the health are whole').toEqual([])
  expect(bar.overflowing, 'nothing runs off the bar').toEqual([])
  // The one crumb between the root and the heading: shortened, its whole name in its tooltip.
  expect(bar.middle.map((m) => m.title)).toEqual([names[G('cross_validation')]])
  expect(bar.middle[0]!.width).toBeGreaterThan(60)
  expect(bar.height, 'at most two lines').toBeLessThanOrEqual(72)
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
      // The bar across the top has no Run: the node's panel runs it, in either view.
      await expect(page.getByRole('banner').getByRole('button', { name: /^Run/ })).toHaveCount(0)
      await expect(panel.getByRole('button', { name: 'Run', exact: true })).toBeEnabled()
      const started = page.waitForResponse((response) => isStartRun(response.request(), verb, nodeId))
      await panel.getByRole('button', { name: 'Run', exact: true }).click()
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

  test(`${view}: read-only mode offers no Run, and says so once`, async ({ page }) => {
    await page.route('**/health', async (route) => {
      const response = await route.fetch()
      await route.fulfill({ json: { ...(await response.json()), read_only: true } })
    })
    const selection = view === 'graph' ? `view=graph&focus=${N('raw_rows')}` : `node=${N('raw_rows')}`
    await page.goto(`/ui/#/assets?pipeline=${FILE}&group=${G('source')}&${selection}`)
    const panel = page.getByRole('complementary', { name: 'raw_rows details' })
    await expect(panel).toBeVisible()
    await expect(panel.getByRole('button', { name: 'Run', exact: true })).toHaveCount(0)
    await expect(page.getByRole('note').filter({ hasText: 'Read-only.' })).toBeVisible()
  })
}

test('the grouped list shows running before the first stream event arrives', async ({ page }) => {
  let release!: () => void
  const held = new Promise<void>((resolve) => { release = resolve })
  await page.route(/\/runs\/[^/]+\/events$/, async (route) => {
    await held
    await route.continue()
  })
  try {
    await page.goto(`/ui/#/assets?pipeline=${FILE}&group=${G('cross_validation')}&node=${N('summary')}`)
    const started = page.waitForResponse((response) => isStartRun(response.request(), 'get', N('summary')))
    await page.getByRole('complementary', { name: 'summary details' }).getByRole('button', { name: 'Run', exact: true }).click()
    expect((await started).status()).toBe(202)
    const row = page.getByRole('row').filter({ hasText: 'summary' })
    await expect(row).toContainText('running')
    await expect(page.getByRole('complementary', { name: 'summary details' })).toContainText('running')
    // The breadcrumb's root is the open pipeline.
    await page.getByRole('navigation', { name: 'Group breadcrumb' }).getByRole('button', { name: FILE, exact: true }).click()
    // A group's counts are the server's (the rollup's entry), never the page's own tally of the
    // run it follows: this run has already ended on the server, so the group reads as recorded.
    const group = page.locator(`tr[data-group="${G('cross_validation')}"]`)
    await expect(group).toContainText('1 failing')
    await expect(group).not.toContainText('running')
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
  const problem = page.getByRole('status').filter({ hasText: 'History is unavailable' })
  await expect(problem).toContainText('Check the metadata volume')
})

test('mixed groups and nodes keep their columns readable at laptop width', async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 800 })
  await page.goto(`/ui/#/assets?pipeline=${FILE}`)
  const table = page.locator('.barca-group-table')
  await expect(table).toBeVisible()
  const widths = await table.locator('th').evaluateAll(cells => cells.map(c => ({ name: c.textContent?.trim(), width: c.getBoundingClientRect().width })))
  for (const cell of widths) {
    if (cell.name === 'Contents') expect(cell.width).toBeGreaterThanOrEqual(140)
    if (cell.name === 'Duration') expect(cell.width).toBeGreaterThanOrEqual(90)
  }
})

test('the bar, the map and a group row state the numbers of one rollup response, each with its unit', async ({ page, request }) => {
  // What `barca status --rollup --json` prints for this scope: the same route, the same document.
  const response = await request.get(`${process.env.BARCA_E2E_API}/state/rollup?files=${FILE}`)
  expect(response.ok()).toBe(true)
  const rollup = await response.json()
  const root = rollup.entries[0]
  expect(root.id).toBe('root')
  // Tasks are nodes, not assets: the two units differ here, which is what an audit found the
  // map and the bar disagreeing about.
  expect(root.nodes).toBeGreaterThan(root.assets)
  const sum = (key: 'nodes' | 'assets') => root.children.reduce((n: number, id: string) => n + rollup.entries.find((e: { id: string }) => e.id === id)[key], 0)
  expect([sum('nodes'), sum('assets')]).toEqual([root.nodes, root.assets])

  await page.goto(`/ui/#/assets?pipeline=${FILE}`)
  // The scope's bar counts nodes, and says so.
  const bar = page.locator('.barca-scope-health .barca-health')
  await expect(bar).toHaveAttribute('title', new RegExp(`^${root.nodes} nodes: ${root.not_fresh} not fresh \\(${root.causes.failing} failing`))
  // The bar is in the map's words and colours: a segment per cause the entry counts, no health word.
  await expect(bar.locator('[data-part="failing"]')).toHaveCount(1)
  await expect(bar).not.toContainText(/behind|never run|blocked/)
  // The map counts nodes too, and says so; the line beside it gives the scope's assets.
  await expect(page.locator('.barca-healthmap-summary')).toContainText(`${root.nodes} nodes`)
  await expect(page.locator('.barca-health-overview-title')).toContainText(`${root.not_fresh} of ${root.nodes} nodes not fresh`)
  await expect(page.locator('.barca-health-overview-sub')).toContainText(`${root.assets} assets`)
  // A group's row is its entry.
  const entry = rollup.entries.find((e: { id: string }) => e.id === G('cross_validation'))
  const row = page.locator(`tr[data-group="${G('cross_validation')}"]`)
  await expect(row.locator('.barca-health')).toHaveAttribute('title', new RegExp(`^${entry.nodes} nodes: ${entry.not_fresh} not fresh \\(${entry.causes.failing} failing`))
  // When the group will be fresh is its entry's too: nothing here has an estimate without a scheduler.
  await expect(row).toContainText(`${entry.unbounded} with no estimate`)
  await expect(row.locator('.gcol-failed')).toHaveText(String(entry.health.failing))
})

// The words the map and the list may say for grey: one, the server's.
const OLD_GREY = ['not known yet', 'waiting', 'needs attention']

test('the map says the scope in the terms of the gap, by cause and in nodes, and a segment is coloured by what is inside it', async ({ page, request }) => {
  const rollup = await (await request.get(`${process.env.BARCA_E2E_API}/state/rollup`)).json()
  const root = rollup.entries[0]
  expect(root.not_fresh).toBeGreaterThan(0)
  await page.goto('/ui/#/assets')
  const overview = page.getByRole('region', { name: 'Health' })
  const map = overview.locator('.barca-healthmap')
  await expect(map.locator('svg')).toBeVisible()
  // Every cause the scope counts is said with its count, in the line beside the map.
  const words: Record<string, string> = { failing: 'failing', overdue: 'overdue', no_refresh_path: 'no refresh path', queued: 'queued', undecided: 'undecided', catching_up: 'catching up' }
  const line = overview.getByLabel('Nodes that are not fresh, by cause')
  for (const [cause, count] of Object.entries(root.causes as Record<string, number>)) {
    if (count > 0) await expect(line).toContainText(`${count} ${words[cause]}`)
  }
  // The centre leads with the same counts; the caption gives the unit and what has no estimate.
  const first = Object.entries(root.causes as Record<string, number>).find(([cause, count]) => count > 0 && ['failing', 'overdue', 'no_refresh_path'].includes(cause))!
  await expect(map.locator('.barca-healthmap-center')).toContainText(`${first[1]} ${words[first[0]]}`)
  await expect(map.locator('.barca-healthmap-summary')).toHaveText(`${root.nodes} nodes, ${root.not_fresh} not fresh`)
  if (root.unbounded > 0) await expect(map.locator('.barca-healthmap-estimate')).toContainText(`${root.unbounded} with no estimate`)
  // Grey has one word, and the key names colours by cause.
  for (const old of OLD_GREY) await expect(overview).not.toContainText(old)
  await expect(map.getByRole('list', { name: /Colours/ })).toContainText('undecided')
  // A group with fresh and not-fresh nodes inside is drawn in both colours, not its worst one.
  const mixed = rollup.entries.find((e: { kind: string; not_fresh: number; nodes: number }) => e.kind === 'group' && e.not_fresh > 0 && e.not_fresh < e.nodes)
  expect(mixed).toBeTruthy()
  const segment = map.locator(`g.barca-healthmap-arc[aria-label^="${mixed.label}:"]`).first()
  await expect(segment).toHaveAttribute('data-part', 'mixed')
  await expect(segment.locator('path[data-part="fresh"]')).toHaveCount(1)
  // A fully fresh group is still on the map, in the fresh colour alone.
  const fresh = rollup.entries.find((e: { kind: string; not_fresh: number; parent: string }) => e.kind === 'group' && e.not_fresh === 0 && e.parent === 'root')
  if (fresh) await expect(map.locator(`g.barca-healthmap-arc[aria-label^="${fresh.label}:"]`).first()).toHaveAttribute('data-part', 'fresh')
})

test('the to-do list holds every not-fresh node under one root; more opens the rest in place; a root lights what it holds and opens its node', async ({ page, request }) => {
  const rollup = await (await request.get(`${process.env.BARCA_E2E_API}/state/rollup`)).json()
  const root = rollup.entries[0]
  const held: { root: string; nodes: number; cumulative_nodes: number }[] = root.held_by
  // INV-HEALTH-04, as served: the scope's roots add up to its not-fresh nodes.
  expect(held.reduce((n, h) => n + h.nodes, 0)).toBe(root.not_fresh)
  expect(held.length).toBeGreaterThan(3)
  expect(held.map((h) => h.root)).toEqual(rollup.roots.map((r: { id: string }) => r.id))

  await page.goto('/ui/#/assets')
  const overview = page.getByRole('region', { name: 'Health' })
  const list = overview.getByRole('list', { name: /What holds the nodes that are not fresh/ })
  // The head of the list, and how much of the scope it explains, without scrolling.
  await expect(list.locator('> li')).toHaveCount(3)
  await expect(overview).toContainText(`The first 3 roots hold ${held[2]!.cumulative_nodes} of ${root.not_fresh} nodes that are not fresh`)
  await expect(list.locator('> li').nth(2)).toBeInViewport()
  // More opens the whole list in place: every root is there, none twice, and the page did not
  // leave for the failures tab.
  await overview.getByRole('button', { name: /more: show all/ }).click()
  const ids = await overview.locator('[data-root]').evaluateAll((items) => items.map((item) => item.getAttribute('data-root')))
  expect([...ids].sort()).toEqual(held.map((h) => h.root).sort())
  await expect(page.getByRole('button', { name: /^Failures/ })).toHaveAttribute('aria-pressed', 'false')
  await expect(page.getByText('Nothing has failed here')).toHaveCount(0)

  // The failing check: its error, what it holds, and the command that retries it, to copy.
  const failure = overview.locator(`[data-root="failure:${N('check__fold_02__model')}"]`)
  await expect(failure).toContainText('failure')
  await expect(failure).toContainText('feature count mismatch')
  await expect(failure).toContainText('holds 1 node (0 assets)')
  await expect(failure.locator('code')).toHaveText('barca run check__fold_02__model --server')
  await expect(failure.getByRole('button', { name: 'Copy command' })).toBeVisible()
  // Picking it lights what it holds: its group's row stays, the others step back.
  await failure.getByRole('button', { name: 'failure', exact: true }).click()
  await expect(failure).toHaveAttribute('data-selected', 'true')
  await expect(page.locator('.barca-group-table tbody tr[data-held="true"]')).toHaveCount(1)
  await expect(page.locator('.barca-group-table tbody tr[data-held="false"]').first()).toBeVisible()
  await expect(overview.locator('.barca-healthmap svg')).toHaveAttribute('data-dimming', 'true')
  // One click on its node's name reaches the node's panel.
  await failure.getByRole('button', { name: 'check__fold_02__model', exact: true }).click()
  await expect(page.getByRole('complementary', { name: 'check__fold_02__model details' })).toBeVisible()
})

test('a node panel says why the node is not fresh, what holds it with a link, and why it has no estimate: the row and the root list, as served', async ({ page, request }) => {
  const api = process.env.BARCA_E2E_API
  const states: { id: string; cause: string | null; root: { id: string; kind: string; node: string } | null; expected_fresh_at: string | null; no_estimate: string | null }[] = await (await request.get(`${api}/state?files=${FILE}`)).json()
  const rollup = await (await request.get(`${api}/state/rollup?files=${FILE}`)).json()
  // Every node that is not fresh has an estimate or a reason, never neither (#968).
  for (const row of states) expect(row.no_estimate !== null).toBe(row.cause !== null && row.expected_fresh_at === null)
  // A node held by another node's root: the panel names the cause in the map's word, links
  // the root's node, and words the root as `barca status --rollup` does.
  // (When every not-fresh node is its own root, the panel says "here" and there is no link.)
  const row = (states.find((s) => s.root !== null && s.root.node !== s.id) ?? states.find((s) => s.root !== null))!
  expect(row).toBeTruthy()
  const held = rollup.roots.find((r: { id: string }) => r.id === row.root!.id)
  const group = (await (await request.get(`${api}/groups`)).json()).find((g: { members: string[] }) => g.members.includes(row.id))
  await page.goto(`/ui/#/assets?pipeline=${FILE}${group ? `&group=${encodeURIComponent(group.id)}` : ''}&node=${encodeURIComponent(row.id)}`)
  const panel = page.getByRole('complementary', { name: /details$/ })
  await expect(panel.locator('[data-cause-line]')).toHaveAttribute('data-cause-line', row.cause!)
  await expect(panel.locator(`[data-root="${row.root!.id}"]`)).toBeVisible()
  await expect(panel.locator('[data-root-detail]')).toHaveText(held.detail)
  await expect(panel.locator('[data-root-action]')).toHaveAttribute('data-root-action', held.action.kind)
  // No scheduler runs here: no estimate, and the reason is the server's word for it.
  await expect(panel.locator('[data-no-estimate]')).toHaveAttribute('data-no-estimate', row.no_estimate!)
  await expect(panel.locator('[data-expected-fresh-at]')).toHaveCount(0)
  if (row.root!.node === row.id) {
    await expect(panel.locator(`[data-root="${row.root!.id}"]`)).toContainText('here')
    return
  }
  // The link opens the root's node.
  await panel.locator(`[data-root="${row.root!.id}"] button`).click()
  await expect(page).toHaveURL(new RegExp(`node=${encodeURIComponent(row.root!.node).replace(/\./g, '\\.')}`))
})
