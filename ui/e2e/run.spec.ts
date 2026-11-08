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

// The bar across the top has no Run: a page's toolbar runs what is open (the pipeline or the
// project), the node panel or graph inspector runs its node, and ⌘K offers either.
test.describe('toolbar Run', () => {
  const toolbarRun = (page: Page) => page.locator('.barca-view-head').getByRole('button', { name: /^Run/ })

  test('names what is open, whatever is selected; the inspector runs the node', async ({ page }) => {
    await page.goto('/ui/#/graph?pipeline=pipeline.py')
    await expect(toolbarRun(page)).toHaveAccessibleName(/^Run pipeline\.py \(\d+ assets?(, \d+ sensors?)?\)$/)
    await page.getByText('say_hello').first().click()
    await expect(toolbarRun(page)).toHaveAccessibleName(/^Run pipeline\.py/)
    await expect(inspector(page).getByRole('button', { name: 'Run', exact: true })).toBeVisible()
    await expect(page.getByRole('banner').getByRole('button', { name: /^Run/ })).toHaveCount(0)
  })

  test('a pipeline gets its file at once', async ({ page }) => {
    await page.goto('/ui/#/assets?pipeline=other.py')
    await expect(toolbarRun(page)).toHaveAccessibleName(/^Run other\.py/)
    const started = page.waitForRequest((r) => r.method() === 'POST' && r.url().endsWith('/runs'))
    await toolbarRun(page).click()
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
    await expect(toolbarRun(page)).toHaveAccessibleName(/^Run all \d+ assets?(, \d+ sensors?)?$/)
    await toolbarRun(page).click()
    const dialog = page.getByRole('dialog')
    await dialog.getByRole('button', { name: 'Cancel' }).click()
    await expect(dialog).toHaveCount(0)
    expect(posts).toEqual([])
    await toolbarRun(page).click()
    await dialog.getByRole('button', { name: /^Run all/ }).click()
    await expect.poll(() => posts).toEqual([{ command: 'get', targets: [] }])
  })

  test('the inspector runs the selected task and streams its output', async ({ page }) => {
    await page.goto('/ui/#/graph')
    await page.getByText('say_hello').first().click()

    const started = page.waitForRequest((r) => isStartRun(r, 'run', 'pipeline.py:say_hello'))
    await inspector(page).getByRole('button', { name: 'Run', exact: true }).click()
    await started

    await expect(page).toHaveURL(/#\/assets\?.*view=graph/) // stays on the graph; no navigation
    await expect(inspector(page)).toContainText('hello from e2e 6', { timeout: 30_000 })
  })

  test('the command palette runs the selected asset', async ({ page }) => {
    await page.goto('/ui/#/graph')
    await page.getByText('numbers').first().click()

    const started = page.waitForRequest((r) => isStartRun(r, 'get', 'pipeline.py:numbers'))
    await page.keyboard.press('ControlOrMeta+k')
    await page.getByRole('combobox', { name: /^Jump to/ }).fill('Run numbers')
    // Enter picks the highlighted result: wait for the list to show it, as a reader would.
    await expect(page.getByRole('option', { name: /^Run numbers/ })).toHaveAttribute('aria-selected', 'true')
    await page.keyboard.press('Enter')
    await started
  })

  test('is on every page: the Tasks page offers to run all assets and sensors', async ({ page }) => {
    await page.goto('/ui/#/tasks?state=all')
    await expect(toolbarRun(page)).toHaveAccessibleName(/^Run all \d+ assets/)
    await expect(toolbarRun(page)).toBeEnabled()
  })
})

test.describe('node panel', () => {
  test('shows the distribution of run durations next to "took"', async ({ page, request }) => {
    // Build a history with a spread of durations.
    await seedRuns(request, 'say_hello', 8)

    await page.goto('/ui/#/tasks?state=all')
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
