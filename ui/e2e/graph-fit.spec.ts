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

test('fit-to-screen zooms out a little, never past 0.8, to show the failures together with room around them', async ({ page }) => {
  for (const box of await page.locator('.react-flow__node').evaluateAll((els) => els.map((el) => el.getBoundingClientRect().width))) {
    expect(box).toBeGreaterThanOrEqual(208 * 0.8 - 0.5)
  }
  const failed = page.locator(node('demand_forecast'))
  // Its failed keys make it failing: the one state shown, instead of `partial`.
  await expect(failed).toContainText('failing')
  // One line a card: the glyph and the name, the state's word in its tooltip and accessible name.
  const name = failed.locator('.barca-dag-name')
  expect(parseFloat(await name.evaluate((el) => getComputedStyle(el).fontSize))).toBeGreaterThanOrEqual(12)
  await expect(failed.locator('.barca-dag')).toHaveAttribute('title', /^failing/)
  // Both failures are on screen, clear of the canvas's edges.
  const canvas = (await page.locator('.react-flow').boundingBox())!
  for (const name of ['demand_forecast', 'publish_plan']) {
    const box = (await page.locator(node(name)).boundingBox())!
    expect(box.x).toBeGreaterThanOrEqual(canvas.x + 24)
    expect(box.x + box.width).toBeLessThanOrEqual(canvas.x + canvas.width - 24)
  }
})

test('explicit fit includes every node at laptop width', async ({ page }) => {
  await page.setViewportSize({ width: 1024, height: 768 })
  await page.getByRole('button', { name: 'Fit to screen' }).click()
  const canvas = (await page.locator('.react-flow').boundingBox())!
  await expect.poll(async () => page.locator('.react-flow__node').evaluateAll((nodes, c) => nodes.every(n => {
    const b = n.getBoundingClientRect()
    return b.left >= c.x && b.right <= c.x + c.width && b.top >= c.y && b.bottom <= c.y + c.height
  }), canvas)).toBe(true)
})

test('a consumer a failure upstream kept from running reads blocked and names the failure in its hint', async ({ page }) => {
  const validate = page.locator(node('validate_publication'))
  await expect(validate).toContainText('blocked')
  await expect(validate.locator('.barca-dag')).toHaveAttribute('data-tone', 'warning')
  await expect(validate.locator('.barca-dag')).toHaveAttribute('title', /Blocked: waits on publish_plan, whose last attempt failed/)
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
