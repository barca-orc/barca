import { expect, test, type Page } from '@playwright/test'
import { describeShifts, slowApi, takeShifts, trackLayoutShift } from './helpers/layoutShift'

// Pages must not move while their data loads. Each flow is measured with the API held back
// 800ms, so the loading state is on screen and the arrival of the data is past the browser's
// 500ms input window. A new page or panel should get a flow here.
const BUDGET = 0.005

async function expectStable(page: Page, what: string) {
  const { total, shifts } = await takeShifts(page, 1500)
  expect(total, `${what} shifted:\n${describeShifts(shifts)}`).toBeLessThanOrEqual(BUDGET)
}

test.beforeEach(async ({ page }) => {
  await trackLayoutShift(page)
  await slowApi(page)
})

test('assets page: loading the table', async ({ page }) => {
  await page.goto('/ui/#/assets')
  await expectStable(page, 'assets page load')
})

test('assets page: opening a node panel', async ({ page }) => {
  await page.goto('/ui/#/assets')
  await expect(page.getByRole('row', { name: /say_hello/ })).toBeVisible()
  await takeShifts(page)
  await page.getByRole('row', { name: /say_hello/ }).click()
  await expectStable(page, 'opening the node panel')
})

test('assets page: switching the node in the panel', async ({ page }) => {
  await page.goto('/ui/#/assets')
  await page.getByRole('row', { name: /say_hello/ }).click()
  await expect(page.locator('.barca-panel')).toContainText('median')
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
