import { expect, test, type Page } from '@playwright/test'
import { seedRuns } from './helpers/seed'
import { describeShifts, slowApi, takeShifts, trackLayoutShift } from './helpers/layoutShift'

// Pages must not move while their data loads. Each flow is measured with the API held back
// 800ms, so the loading state is on screen and the arrival of the data is past the browser's
// 500ms input window. A new page or panel should get a flow here.
const BUDGET = 0.005

async function expectStable(page: Page, what: string) {
  const { total, shifts } = await takeShifts(page, 1500)
  expect(total, `${what} shifted:\n${describeShifts(shifts)}`).toBeLessThanOrEqual(BUDGET)
}

// A node with history is the case that matters: the panel has stats, a histogram and a run list.
test.beforeAll(async ({ request }) => {
  await seedRuns(request, 'say_hello', 6)
  await seedRuns(request, 'pipeline.py:history_asset', 2, 'get')
  // A failed task, so a table has an error line to measure.
  await seedRuns(request, 'history_failure', 1)
})

test.beforeEach(async ({ page }) => {
  await trackLayoutShift(page)
  await slowApi(page)
})

test('assets page: loading the table', async ({ page }) => {
  await page.goto('/ui/#/assets')
  await expectStable(page, 'assets page load')
})

test('tasks page: opening a node panel', async ({ page }) => {
  await page.goto('/ui/#/tasks?state=all')
  await expect(page.getByRole('row', { name: /say_hello/ })).toBeVisible()
  await takeShifts(page)
  await page.getByRole('row', { name: /say_hello/ }).click()
  await expectStable(page, 'opening the node panel')
})

test('assets page: switching the node in the panel', async ({ page }) => {
  await page.goto('/ui/#/assets')
  await page.getByRole('row', { name: /history_asset.*pipeline.py/ }).click()
  await expect(page.getByRole('complementary', { name: /details$/ })).toContainText('median')
  await takeShifts(page)
  await page.getByRole('row', { name: /numbers/ }).click()
  await expectStable(page, 'switching the panel to another node')
})

test('graph page: loading and selecting a node', async ({ page }) => {
  await page.goto('/ui/#/graph')
  await expectStable(page, 'graph page load')
  await page.getByText('say_hello').first().click()
  await expectStable(page, 'selecting a graph node')
})

test('schedules page: loading', async ({ page }) => {
  await page.goto('/ui/#/schedules')
  await expectStable(page, 'schedules page load')
})

// Typography and layout (#765). The panel beside a table must not make its headers wrap or
// its rows grow; rows are the compact 44px, an error line is 13px in the text colour (only a
// state's glyph is coloured, never its error), and a
// name is one text node, so selecting it copies it whole. (The grouped page's header has its
// own check in e2e-groups/groups.spec.ts, against a project declared in groups.)
test('tasks page: compact rows and one-line headers beside the panel', async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  await page.goto('/ui/#/tasks?state=all')
  // The row's name: a click on its error line opens the traceback instead, by design.
  await page.getByRole('row', { name: /history_failure/ }).locator('.name').click()
  await expect(page.getByRole('complementary', { name: /details$/ })).toBeVisible()
  const table = await page.locator('.barca-table').evaluate((el) => {
    const probe = document.createElement('span')
    probe.style.color = 'var(--text-default)'
    document.body.append(probe)
    const text = getComputedStyle(probe).color
    probe.remove()
    const error = el.querySelector('.barca-error-line')!
    return {
      headers: [...el.querySelectorAll('th')].map((th) => th.getBoundingClientRect().height),
      rows: [...el.querySelectorAll('tbody tr:not(.barca-table-spacer)')].map((tr) => tr.getBoundingClientRect().height),
      error: { size: getComputedStyle(error).fontSize, text: getComputedStyle(error).color === text },
    }
  })
  for (const height of table.headers) expect(height, 'a header on one line').toBeLessThanOrEqual(36)
  for (const height of table.rows) expect(height, 'a compact row').toBeLessThanOrEqual(45)
  expect(table.error).toEqual({ size: '13px', text: true })
  const copied = await page.locator('tr[aria-selected="true"] .barca-mid').evaluate((el) => {
    const range = document.createRange()
    range.selectNodeContents(el)
    const selection = getSelection()!
    selection.removeAllRanges()
    selection.addRange(range)
    return selection.toString()
  })
  expect(copied).toBe('history_failure')
})
