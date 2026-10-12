import { expect, test } from '@playwright/test'

test('primary sections are separate from the Assets view toggle', async ({ page }) => {
  await page.goto('/ui/#/assets')
  const navigation = page.getByRole('navigation', { name: 'Main navigation' })
  await expect(navigation.getByRole('link')).toHaveText(['Assets', 'Runs', 'Schedules', 'Tasks', 'Sensors'])
  await expect(navigation.getByRole('link', { name: 'Assets', exact: true })).toHaveAttribute('aria-current', 'page')
  const toggle = page.getByRole('group', { name: 'Asset view' })
  await expect(toggle.getByRole('button', { name: 'List', exact: true })).toHaveAttribute('aria-pressed', 'true')

  await page.getByRole('button', { name: /^pipeline.py/ }).click()
  const row = page.getByRole('row', { name: /numbers/ })
  await row.click()
  await expect(row).toHaveAttribute('aria-selected', 'true')
  await toggle.getByRole('button', { name: 'Graph', exact: true }).click()
  await expect(page.locator('.react-flow__node')).toHaveCount(5)
  await expect(page.locator('.react-flow__node[data-id="pipeline.py:numbers"]')).toHaveClass(/selected/)
  await expect(navigation.getByRole('link', { name: 'Assets', exact: true })).toHaveAttribute('aria-current', 'page')
  await toggle.getByRole('button', { name: 'List', exact: true }).click()
  await expect(row).toHaveAttribute('aria-selected', 'true')
  await expect(page.getByRole('complementary', { name: 'numbers details' })).toBeVisible()

  await navigation.getByRole('link', { name: 'Tasks', exact: true }).click()
  await expect(page.locator('.barca-table tbody tr')).toHaveCount(2)
  await expect(page.getByRole('row', { name: /say_hello/ })).toBeVisible()
  await expect(toggle).toHaveCount(0)
  await navigation.getByRole('link', { name: 'Sensors', exact: true }).click()
  await expect(page.locator('.barca-table tbody tr')).toHaveCount(1)
  await expect(page.getByRole('row', { name: /history_clock/ })).toBeVisible()
})

test('old graph links open Assets in graph view with the selected node', async ({ page }) => {
  await page.goto('/ui/#/graph?pipeline=pipeline.py&focus=pipeline.py%3Anumbers')
  await expect(page).toHaveURL(/#\/assets\?.*view=graph/)
  await expect(page.getByRole('button', { name: 'Graph', exact: true })).toHaveAttribute('aria-pressed', 'true')
  await expect(page.locator('.react-flow__node[data-id="pipeline.py:numbers"]')).toHaveClass(/selected/)
  await page.reload()
  await expect(page.locator('.react-flow__node')).toHaveCount(5)
})
