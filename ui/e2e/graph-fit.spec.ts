import { test, expect } from '@playwright/test'
import { blockedBy, served } from './helpers/served'

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
  return served({
    ...a, name, partitioned,
    cache: { state: never ? 'never_run' : failed ? 'partial' : 'cached', reason: 'materialized', detail: 'fixture' },
    partitions: partitioned ? { total: 3077, cached: failed ? 3070 : 3077, missing: failed ? 7 : 0, missing_keys: [], failed: failed ? 7 : 0, failed_keys: [], changed: {}, behind: 0 } : null,
    last_materialization: never ? null : materialization(failed || name === 'publish_plan' ? 'failed' : 'success', name === 'publish_plan' ? 'warehouse refused the plan' : null),
    shape: null, durations: null, next_run: null, last_success_at: null, failing_since: null, consecutive_failures: 0, running: null,
    // The server's answer: the validation is blocked by the failing publish step above it.
    ...(never ? blockedBy(id('publish_plan')) : {}),
  })
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

/** The zoom of the canvas, and the smallest text any card draws, as rendered on screen (px). */
async function rendered(page: import('@playwright/test').Page) {
  return page.evaluate(() => {
    const zoom = new DOMMatrix(getComputedStyle(document.querySelector('.react-flow__viewport')!).transform).a
    const sizes = [...document.querySelectorAll('.react-flow__node *')]
      .filter((el) => [...el.childNodes].some((c) => c.nodeType === Node.TEXT_NODE && c.textContent!.trim()) && !el.closest('.barca-sr-only'))
      .map((el) => parseFloat(getComputedStyle(el).fontSize))
    return { zoom, smallest: Math.min(...sizes) * zoom }
  })
}

/** Edges (by id) whose drawn path passes through a card other than their own ends. */
async function edgesThroughCards(page: import('@playwright/test').Page) {
  return page.evaluate(() => {
    const cards = [...document.querySelectorAll('.react-flow__node')].map((n) => ({ id: n.getAttribute('data-id'), r: n.getBoundingClientRect() }))
    const out: string[] = []
    for (const edge of document.querySelectorAll<SVGGElement>('.react-flow__edge')) {
      if (edge.style.visibility === 'hidden') continue
      const path = edge.querySelector<SVGPathElement>('path.react-flow__edge-path')
      if (!path) continue
      const ctm = path.getScreenCTM()!
      for (let s = 2; s < path.getTotalLength() - 2; s += 3) {
        const p = path.getPointAtLength(s).matrixTransform(ctm)
        const hit = cards.find((c) => p.x > c.r.left + 1 && p.x < c.r.right - 1 && p.y > c.r.top + 1 && p.y < c.r.bottom - 1)
        if (hit) { out.push(`${edge.getAttribute('data-id')} through ${hit.id}`); break }
      }
    }
    return out
  })
}

test('fit-to-screen zooms out only as far as 11px labels, and centres the first failure with room around it', async ({ page }) => {
  // The smallest text on a card (a kind, 12px) never renders below 11px.
  const { zoom, smallest } = await rendered(page)
  expect(zoom).toBeGreaterThanOrEqual(11 / 12 - 1e-6)
  expect(smallest).toBeGreaterThanOrEqual(11 - 1e-6)
  const failed = page.locator(node('demand_forecast'))
  // Its failed keys make it failing: the one state shown, instead of `partial`.
  await expect(failed).toContainText('failing')
  // One line a card: the glyph and the name, the state's word in its tooltip and accessible name.
  const name = failed.locator('.barca-dag-name')
  expect(parseFloat(await name.evaluate((el) => getComputedStyle(el).fontSize))).toBeGreaterThanOrEqual(12)
  await expect(failed.locator('.barca-dag')).toHaveAttribute('title', /^failing/)
  // The two failures are too far apart to show together at that zoom: the first is on screen,
  // clear of the canvas's edges, and the other a pan away, not shrunk into view.
  const canvas = (await page.locator('.react-flow').boundingBox())!
  const box = (await failed.boundingBox())!
  expect(box.x).toBeGreaterThanOrEqual(canvas.x + 24)
  expect(box.x + box.width).toBeLessThanOrEqual(canvas.x + canvas.width - 24)
})

test('at laptop width a fit never shrinks the labels: it shows what fits around the failures, and the rest is a pan away', async ({ page }) => {
  await page.setViewportSize({ width: 1024, height: 768 })
  await page.reload()
  await expect(page.locator('.react-flow__node')).toHaveCount(9)
  await page.waitForTimeout(600)
  expect((await rendered(page)).smallest).toBeGreaterThanOrEqual(11 - 1e-6)
  const canvas = (await page.locator('.react-flow').boundingBox())!
  const box = (await page.locator(node('demand_forecast')).boundingBox())!
  expect(box.x).toBeGreaterThanOrEqual(canvas.x)
  expect(box.x + box.width).toBeLessThanOrEqual(canvas.x + canvas.width)
})

test('edges are routed around the cards, never through one', async ({ page }) => {
  expect(await edgesThroughCards(page)).toEqual([])
  await page.getByRole('button', { name: 'Top-down layout' }).click()
  await page.waitForTimeout(600)
  expect(await edgesThroughCards(page)).toEqual([])
})

test('opening a node pans the graph so the panel does not cover it', async ({ page }) => {
  await page.setViewportSize({ width: 1024, height: 768 })
  await page.getByRole('button', { name: 'Fit to screen' }).click()
  await page.waitForTimeout(400)
  // Pan so a node sits at the right edge of the canvas, where the panel opens, then open it.
  const canvas = (await page.locator('.react-flow').boundingBox())!
  const target = page.locator(node('promo_plan'))
  const before = (await target.boundingBox())!
  await page.mouse.move(canvas.x + 40, canvas.y + canvas.height - 120)
  await page.mouse.down()
  await page.mouse.move(canvas.x + 40 + (canvas.x + canvas.width - 220 - before.x), canvas.y + canvas.height - 120, { steps: 8 })
  await page.mouse.up()
  await target.click()
  await expect(page.getByRole('complementary', { name: /promo_plan/ })).toBeVisible()
  const narrowed = (await page.locator('.react-flow').boundingBox())!
  expect(narrowed.width).toBeLessThan(canvas.width)
  await expect.poll(async () => {
    const box = (await target.boundingBox())!
    return box.x >= narrowed.x && box.x + box.width <= narrowed.x + narrowed.width
  }).toBe(true)
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
