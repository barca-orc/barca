import { expect, test, type Page } from '@playwright/test'
import { isStartRun, seedRuns } from './helpers/seed'

const inspector = (page: Page) => page.getByRole('complementary', { name: /inspector$/ })

test.describe('graph page', () => {
  test('inspector `run` on a task streams its output', async ({ page }) => {
    await page.goto('/ui/#/graph')
    await page.getByText('say_hello').first().click()

    const started = page.waitForRequest((r) => isStartRun(r, 'run'))
    await inspector(page).getByRole('button', { name: 'Run', exact: true }).click()
    await started

    await expect(inspector(page)).toContainText('hello from e2e 6', { timeout: 30_000 })
    await expect(inspector(page)).not.toContainText('could not start')
  })
})

test.describe('topbar Run', () => {
  const topbarRun = (page: Page) => page.getByRole('banner').getByRole('button', { name: /^Run/ })

  test('names what it runs: the selected node, else the open pipeline', async ({ page }) => {
    await page.goto('/ui/#/graph?pipeline=pipeline.py')
    await expect(topbarRun(page)).toHaveText(/^Run pipeline\.py \(\d+ nodes?\)$/)
    await page.getByText('say_hello').first().click()
    await expect(topbarRun(page)).toHaveText('Run say_hello')
  })

  test('a pipeline gets its file at once', async ({ page }) => {
    await page.goto('/ui/#/assets?pipeline=other.py')
    await expect(topbarRun(page)).toHaveText(/^Run other\.py/)
    const started = page.waitForRequest((r) => r.method() === 'POST' && r.url().endsWith('/runs'))
    await topbarRun(page).click()
    expect((await started).postDataJSON()).toMatchObject({ command: 'get', targets: [], files: ['other.py'] })
  })

  test('all pipelines asks first, and Cancel starts nothing', async ({ page }) => {
    const posts: unknown[] = []
    await page.route('**/runs', (route) => {
      if (route.request().method() !== 'POST') return route.continue()
      posts.push(route.request().postDataJSON())
      return route.fulfill({ status: 202, json: { run_id: 'r1', status: 'running', position: 0 } })
    })
    await page.goto('/ui/#/assets')
    await expect(topbarRun(page)).toHaveText(/^Run all \d+ nodes?$/)
    await topbarRun(page).click()
    const dialog = page.getByRole('dialog')
    await dialog.getByRole('button', { name: 'Cancel' }).click()
    await expect(dialog).toHaveCount(0)
    expect(posts).toEqual([])
    await topbarRun(page).click()
    await dialog.getByRole('button', { name: /^Run all/ }).click()
    await expect.poll(() => posts).toEqual([{ command: 'get', targets: [] }])
  })

  test('runs the selected task and streams its output', async ({ page }) => {
    await page.goto('/ui/#/graph')
    await page.getByText('say_hello').first().click()

    const started = page.waitForRequest((r) => isStartRun(r, 'run', 'pipeline.py:say_hello'))
    await topbarRun(page).click()
    await started

    await expect(page).toHaveURL(/#\/assets\?.*view=graph/) // stays on the graph; no navigation
    await expect(inspector(page)).toContainText('hello from e2e 6', { timeout: 30_000 })
  })

  test('gets the selected asset', async ({ page }) => {
    await page.goto('/ui/#/graph')
    await page.getByText('numbers').first().click()

    const started = page.waitForRequest((r) => isStartRun(r, 'get', 'pipeline.py:numbers'))
    await topbarRun(page).click()
    await started
  })

  test('is absent on pages with nothing to run', async ({ page }) => {
    await page.goto('/ui/#/tasks')
    await expect(page.getByRole('banner')).toBeVisible()
    await expect(topbarRun(page)).toHaveCount(0)
  })
})

test.describe('node panel', () => {
  test('shows the distribution of run durations next to "took"', async ({ page, request }) => {
    // Build a history with a spread of durations.
    await seedRuns(request, 'say_hello', 8)

    await page.goto('/ui/#/tasks')
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
