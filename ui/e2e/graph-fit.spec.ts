import { test, expect } from '@playwright/test'

// The planning fixture of the 0.38 UX review: 3,077 accounts, 7 keys failing, a failing
// publish step, and a validation that cannot run because of it.
const NAMES = ['fiscal_calendar', 'price_list', 'retailer_accounts', 'demand_forecast', 'promo_plan', 'replenishment', 'network_summary', 'publish_plan', 'validate_publication']
const INPUTS: Record<string, string[]> = {
  demand_forecast: ['retailer_accounts', 'fiscal_calendar'], promo_plan: ['demand_forecast', 'price_list'], replenishment: ['promo_plan'],
  network_summary: ['replenishment'], publish_plan: ['network_summary'], validate_publication: ['publish_plan'],
}
const id = (name: string) => `planning.py:${name}`
const assets = NAMES.map((name) => ({ id: id(name), kind: name.startsWith('validate') ? 'task' : 'asset', freshness: 'always', env: [], inputs: (INPUTS[name] ?? []).map(id) }))
const materialization = (status: 'success' | 'failed', error: string | null = null) =>
  ({ status, error, created_at: '2026-10-31T00:00:00Z', elapsed_seconds: 1, run_hash: null, artifact: null, format: null, size_bytes: null })
const states = assets.map((a) => {
  const name = a.id.split(':')[1]
  const partitioned = ['demand_forecast', 'promo_plan', 'replenishment'].includes(name)
  const failed = name === 'demand_forecast'
  const never = name === 'validate_publication'
  return {
    ...a, name, partitioned,
    cache: { state: never ? 'never_run' : failed ? 'partial' : 'cached', reason: 'materialized', detail: 'fixture' },
    partitions: partitioned ? { total: 3077, cached: failed ? 3070 : 3077, missing: failed ? 7 : 0, missing_keys: [], failed: failed ? 7 : 0, failed_keys: [], changed: {} } : null,
    last_materialization: never ? null : materialization(failed || name === 'publish_plan' ? 'failed' : 'success', name === 'publish_plan' ? 'warehouse refused the plan' : null),
    shape: null, durations: null, next_run: null, last_success_at: null, failing_since: null, consecutive_failures: 0, running: null,
  }
})

const node = (name: string) => `.react-flow__node[data-id="${id(name)}"]`

test.use({ viewport: { width: 1440, height: 900 } })

test.beforeEach(async ({ page }) => {
  await page.route('**/assets', (route) => route.fulfill({ json: assets }))
  await page.route('**/groups', (route) => route.fulfill({ json: [] }))
  await page.route(/\/state(?:\?.*)?$/, (route) => route.fulfill({ json: states }))
  await page.goto('/ui/#/assets?view=graph')
  await expect(page.locator('.react-flow__node')).toHaveCount(9)
  // The settle delay before the first fit, and its animation.
  await page.waitForTimeout(600)
})

test('fit-to-screen stays readable: cards at 1:1, centred on the first failure', async ({ page }) => {
  // The chain is wider than the canvas, so the fit cannot show it whole: it keeps 1:1 instead.
  for (const box of await page.locator('.react-flow__node').evaluateAll((els) => els.map((el) => el.getBoundingClientRect().width))) {
    expect(box).toBeGreaterThanOrEqual(208)
  }
  const failed = page.locator(node('demand_forecast'))
  await expect(failed).toContainText('7 of 3,077 keys failed')
  const label = failed.getByText('7 of 3,077 keys failed')
  expect(parseFloat(await label.evaluate((el) => getComputedStyle(el).fontSize))).toBeGreaterThanOrEqual(12)
  // Centred on the failed node upstream, which is on screen; the one downstream is not.
  const canvas = (await page.locator('.react-flow').boundingBox())!
  const box = (await failed.boundingBox())!
  expect(box.x).toBeGreaterThan(canvas.x)
  expect(box.x + box.width).toBeLessThan(canvas.x + canvas.width)
  expect(Math.abs(box.x + box.width / 2 - (canvas.x + canvas.width / 2))).toBeLessThan(canvas.width / 4)
  const publish = (await page.locator(node('publish_plan')).boundingBox())!
  expect(publish.x).toBeGreaterThan(canvas.x + canvas.width)
})

test('a consumer that did not run names the failure upstream', async ({ page }) => {
  await expect(page.locator(node('validate_publication'))).toContainText('Blocked: publish_plan failed')
})

test('arrow keys move the selection and Enter opens the panel', async ({ page }) => {
  await page.locator(node('demand_forecast')).focus()
  await page.keyboard.press('Enter')
  await expect(page.getByRole('complementary', { name: 'demand_forecast inspector' })).toBeVisible()
  await expect(page).toHaveURL(/focus=planning\.py(%3A|:)demand_forecast/)
  await page.keyboard.press('ArrowRight')
  await expect(page.getByRole('complementary', { name: 'promo_plan inspector' })).toBeVisible()
  await expect(page.locator(node('promo_plan'))).toBeFocused()
  // Through replenishment and network_summary to publish_plan.
  for (let i = 0; i < 3; i++) await page.keyboard.press('ArrowRight')
  // Focus pans to a node that was off screen.
  await expect(page.getByRole('complementary', { name: 'publish_plan inspector' })).toBeVisible()
  const canvas = (await page.locator('.react-flow').boundingBox())!
  await expect
    .poll(async () => {
      const box = (await page.locator(node('publish_plan')).boundingBox())!
      return box.x > canvas.x && box.x + box.width < canvas.x + canvas.width
    })
    .toBe(true)
  await page.keyboard.press('Escape')
  await expect(page.getByRole('complementary', { name: /inspector/ })).toHaveCount(0)
})

test('with nothing selected, an arrow key starts at the first failed node', async ({ page }) => {
  await page.locator('.react-flow__pane').click({ position: { x: 40, y: 40 } })
  await page.keyboard.press('ArrowDown')
  await expect(page.getByRole('complementary', { name: 'demand_forecast inspector' })).toBeVisible()
  await expect(page.locator(node('demand_forecast'))).toBeFocused()
})
