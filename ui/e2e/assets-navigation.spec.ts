import { expect, test } from '@playwright/test'

test('arrow keys move the selection and detail panel without wrapping', async ({ page }) => {
  await page.goto('/ui/#/assets')
  const rows = page.locator('.barca-table tbody tr')
  await expect(rows).toHaveCount(3)
  await rows.first().click()
  await expect(rows.first()).toHaveAttribute('aria-selected', 'true')
  await page.keyboard.press('ArrowUp')
  await expect(rows.first()).toHaveAttribute('aria-selected', 'true')

  for (let index = 1; index < 3; index++) {
    await page.keyboard.press('ArrowDown')
    await expect(rows.nth(index)).toHaveAttribute('aria-selected', 'true')
    await expect(rows.nth(index)).toBeFocused()
    const name = await rows.nth(index).locator('.name').innerText()
    await expect(page.getByRole('complementary', { name: `${name} details` })).toBeVisible()
  }
  await page.keyboard.press('ArrowDown')
  await expect(rows.last()).toHaveAttribute('aria-selected', 'true')
  await page.keyboard.press('ArrowUp')
  await expect(rows.nth(1)).toHaveAttribute('aria-selected', 'true')
  await page.keyboard.press('Escape')
  await expect(page.locator('.barca-table tr[aria-selected="true"]')).toHaveCount(0)
  await expect(page.locator('.barca-sidepanel')).toHaveCount(0)
})

test('navigation follows filtered sort order and leaves form controls alone', async ({ page }) => {
  await page.goto('/ui/#/assets')
  const rows = page.locator('.barca-table tbody tr')
  const search = page.getByPlaceholder('Search by name or file')
  await search.fill('history')
  await expect(rows).toHaveCount(2)
  await page.getByRole('button', { name: 'Name', exact: true }).click()
  await expect(page.getByRole('columnheader', { name: 'Name' })).toHaveAttribute('aria-sort', 'ascending')
  await page.getByRole('button', { name: 'Name', exact: true }).click()
  await expect(page.getByRole('columnheader', { name: 'Name' })).toHaveAttribute('aria-sort', 'descending')
  await rows.first().click()
  await expect(rows.first()).toHaveAttribute('aria-selected', 'true')
  await page.keyboard.press('ArrowDown')
  await expect(rows.nth(1)).toHaveAttribute('aria-selected', 'true')

  await search.focus()
  await page.keyboard.press('ArrowDown')
  await expect(rows.nth(1)).toHaveAttribute('aria-selected', 'true')
  await expect(search).toBeFocused()
  // Navigation also works after interacting with the open detail panel.
  await page.getByRole('button', { name: 'Close (Esc)' }).focus()
  await page.keyboard.press('ArrowDown')
  await expect(rows.nth(1)).toHaveAttribute('aria-selected', 'true')
  await page.keyboard.press('ArrowUp')
  await expect(rows.first()).toHaveAttribute('aria-selected', 'true')
})
