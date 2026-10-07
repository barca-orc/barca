import { expect, test, type Page } from '@playwright/test'
import { seedRuns } from './helpers/seed'

const inspector = (page: Page) => page.getByRole('complementary', { name: /inspector$/ })

test.describe('graph page', () => {
  test('inspector `run` on a task streams its output', async ({ page }) => {
    await page.goto('/ui/#/graph')
    await page.getByText('say_hello').first().click()

    const started = page.waitForRequest((r) => r.method() === 'POST' && r.url().includes('/run/'))
    await inspector(page).getByRole('button', { name: 'run' }).click()
    await started

    await expect(inspector(page)).toContainText('hello from e2e 6', { timeout: 30_000 })
    await expect(inspector(page)).not.toContainText('could not start')
  })
})

test.describe('topbar Run', () => {
  const topbarRun = (page: Page) => page.getByRole('banner').getByRole('button', { name: 'Run' })

  test('is disabled until a node is selected', async ({ page }) => {
    await page.goto('/ui/#/graph')
    await expect(topbarRun(page)).toBeDisabled()
    await page.getByText('say_hello').first().click()
    await expect(topbarRun(page)).toBeEnabled()
  })

  test('runs the selected task and streams its output', async ({ page }) => {
    await page.goto('/ui/#/graph')
    await page.getByText('say_hello').first().click()

    const started = page.waitForRequest((r) => r.method() === 'POST' && r.url().endsWith('/run/say_hello'))
    await topbarRun(page).click()
    await started

    await expect(page).toHaveURL(/#\/graph/) // stays on the graph; no navigation
    await expect(inspector(page)).toContainText('hello from e2e 6', { timeout: 30_000 })
  })

  test('gets the selected asset', async ({ page }) => {
    await page.goto('/ui/#/graph')
    await page.getByText('numbers').first().click()

    const started = page.waitForRequest((r) => r.method() === 'POST' && r.url().endsWith('/get/numbers'))
    await topbarRun(page).click()
    await started
  })

  test('is absent on pages with nothing to run', async ({ page }) => {
    await page.goto('/ui/#/assets')
    await expect(page.getByRole('banner')).toBeVisible()
    await expect(topbarRun(page)).toHaveCount(0)
  })
})

test.describe('node panel', () => {
  test('shows the distribution of run durations next to "took"', async ({ page, request }) => {
    // Build a history with a spread of durations.
    await seedRuns(request, 'say_hello', 8)

    await page.goto('/ui/#/assets')
    await page.getByRole('row', { name: /say_hello/ }).click()

    const hist = page.locator('.barca-hist')
    await expect(hist).toBeVisible()
    // The slot is there from the start (a skeleton); the bars arrive with the run history.
    await expect.poll(() => hist.locator('.barca-hist-bars span').count()).toBeGreaterThan(1)
    await expect(hist.locator('.is-last')).toHaveCount(1)
    // A task never uses the cache, so the panel has no cache-hit stat for it.
    await expect(page.getByRole('complementary', { name: /details$/ }).locator('.barca-stats')).not.toContainText('cache hits')
    await hist.scrollIntoViewIfNeeded()
    await page.screenshot({ path: 'test-results/duration-histogram.png' })
  })
})
