import { expect, test } from '@playwright/test'

test('an empty project explains how to add a node on Runs and graph', async ({ page }) => {
  await page.route('**/assets', route => route.fulfill({ json: [] }))
  await page.route('**/groups', route => route.fulfill({ json: [] }))
  await page.route(/\/state(?:\?.*)?$/, route => route.fulfill({ json: [] }))
  await page.route(/\/runs(?:\?.*)?$/, route => route.fulfill({ json: { runs: [], total: 0, truncated: false } }))
  await page.goto('/ui/#/runs')
  await expect(page.getByText('No runs yet', { exact: true })).toBeVisible()
  await expect(page.getByText(/Add a function decorated with/)).toBeVisible()
  await expect(page.getByText("No run but the schedule's.", { exact: true })).toHaveCount(0)
  await page.goto('/ui/#/assets?view=graph')
  await expect(page.getByText('No assets to graph', { exact: true })).toBeVisible()
})

test('theme follows the OS until an explicit choice is saved', async ({ page }) => {
  await page.emulateMedia({ colorScheme: 'light' })
  await page.goto('/ui/#/assets')
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'light')
  await page.emulateMedia({ colorScheme: 'dark' })
  await expect(page.locator('html')).not.toHaveAttribute('data-theme', 'light')
  await page.getByRole('button', { name: 'Switch to light', exact: true }).click()
  await page.reload()
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'light')
})

test('Runs does not fetch whole asset state for its sidebar', async ({ page }) => {
  const requests: string[] = []
  page.on('request', request => requests.push(new URL(request.url()).pathname))
  await page.goto('/ui/#/runs')
  await expect(page.getByRole('heading', { name: 'Runs', exact: true })).toBeVisible()
  await page.waitForTimeout(11_000)
  expect(requests).not.toContain('/state')
})
