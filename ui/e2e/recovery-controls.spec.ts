import { test, expect } from '@playwright/test'

test('incident controls put replacement data beside containment', async ({ page }) => {
  await page.route(/\/assets\/[^/]+\/versions/, route => route.fulfill({ json: { node_id: 'pipeline.py:numbers', pinnable: true, pin: null, versions: [] } }))
  await page.goto('/ui/#/assets?node=pipeline.py%3Anumbers')
  const panel = page.getByRole('complementary', { name: 'numbers details' })
  await panel.getByRole('tab', { name: 'Incident', exact: true }).click()
  await expect(panel.getByRole('button', { name: 'Upload replacement…' })).toBeVisible()
  await expect(panel.getByRole('button', { name: 'Pause this asset and downstream' })).toBeVisible()
})
