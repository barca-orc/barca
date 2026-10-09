import { test, expect } from '@playwright/test'

test('unloaded sources and dependents remain visible beside healthy graph nodes', async ({ page }) => {
  await page.route('**/health', route => route.fulfill({ json: {
    status: 'ok', version: '0.20.1', read_only: false, scheduler: true,
    load_errors: [
      { file: 'broken.py', error: 'syntax error', affected_nodes: [] },
      { file: 'pipeline.py', error: 'upstream is not loaded', affected_nodes: ['pipeline.py:blocked'] },
    ],
  } }))
  await page.route('**/assets', route => route.fulfill({ json: [
    { id: 'pipeline.py:healthy', kind: 'asset', freshness: { type: 'Always' }, env: [], inputs: [] },
  ] }))
  await page.route('**/state', route => route.fulfill({ json: [] }))
  await page.goto('/ui/#/graph')
  const alert = page.getByRole('alert')
  await expect(alert).toContainText('broken.py')
  await expect(alert).toContainText('syntax error')
  await expect(alert).toContainText('pipeline.py:blocked')
  await expect(page.locator('.react-flow__node')).toHaveCount(1)
  await expect(page.locator('.react-flow__node')).toContainText('healthy')
})
