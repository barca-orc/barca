import { expect, test } from '@playwright/test'

test('the index keeps a deep link\'s filters', async ({ page }) => {
  await page.goto('/ui/#/?pipeline=pipeline.py&show=failures')
  await expect(page).toHaveURL(/#\/assets\?pipeline=pipeline\.py&show=failures$/)
})

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
  await page.getByRole('button', { name: /^Show all/ }).click()
  await expect(page).toHaveURL(/state=all/)
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

test('the table starts within 120px of the top', async ({ page }) => {
  await page.goto('/ui/#/assets')
  await expect(page.getByRole('table')).toBeVisible()
  const box = await page.getByRole('table').boundingBox()
  expect(box!.y).toBeLessThanOrEqual(120)
})

test('idle furniture stays hidden and the version is on the connection dot', async ({ page }) => {
  await page.goto('/ui/#/runs')
  await expect(page.getByRole('heading', { name: 'Runs', exact: true })).toBeVisible()
  await expect(page.locator('.barca-conn')).toHaveAttribute('title', /^barca serve · v/)
  await expect(page.getByRole('region', { name: 'Queue' })).toHaveCount(0)
})

test('the sidebar collapses, and stays collapsed across a reload', async ({ page }) => {
  await page.goto('/ui/#/assets')
  await page.getByRole('button', { name: 'Collapse sidebar' }).click()
  await expect(page.locator('.barca-sidebar')).toHaveAttribute('data-collapsed', 'true')
  await page.reload()
  await page.getByRole('button', { name: 'Expand sidebar' }).click()
  await expect(page.locator('.barca-sidebar')).not.toHaveAttribute('data-collapsed', 'true')
})

test('the command palette jumps to a node', async ({ page }) => {
  await page.goto('/ui/#/runs')
  await expect(page.getByRole('button', { name: /^Search/ })).toBeVisible()
  await page.keyboard.press('ControlOrMeta+k')
  await page.getByRole('combobox', { name: /^Jump to/ }).fill('numbers')
  await expect(page.getByRole('option', { name: /numbers/ }).first()).toBeVisible()
  await page.keyboard.press('Enter')
  await expect(page).toHaveURL(/#\/assets\?pipeline=pipeline\.py&node=pipeline\.py%3Anumbers/)
  await expect(page.getByRole('complementary', { name: 'numbers details' })).toBeVisible()
})

test('the node panel names the definition', async ({ page }) => {
  await page.goto('/ui/#/assets?pipeline=pipeline.py&node=pipeline.py%3Anumbers')
  await expect(page.getByRole('complementary', { name: 'numbers details' }).getByText(/pipeline\.py:\d+/)).toBeVisible()
})

test('run filters survive a reload', async ({ page }) => {
  await page.goto('/ui/#/runs?status=failed&q=zzz')
  await page.reload()
  await expect(page.getByPlaceholder(/filter|search/i).first()).toHaveValue('zzz')
  await expect(page).toHaveURL(/status=failed&q=zzz/)
})
