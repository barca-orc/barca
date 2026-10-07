import { expect, test } from '@playwright/test'

// The kit page lists every primitive in components/ in each state. It is development-only, so
// it exists under `pnpm dev` (which this suite runs against) and not in the built UI.
test('kit page renders every primitive', async ({ page }) => {
  await page.goto('/ui/#/kit')
  for (const title of [
    'Button',
    'IconButton',
    'Tag',
    'StatusDot / StatusBadge',
    'ConnectionBadge',
    'SearchInput / Select / Chip',
    'Skeleton',
    'SidePanel / Section / KeyValue',
  ]) {
    await expect(page.getByRole('heading', { name: title, exact: true })).toBeVisible()
  }
  // A chip is a toggle: pressing it flips aria-pressed.
  const chip = page.getByRole('button', { name: /assets/ })
  await expect(chip).toHaveAttribute('aria-pressed', 'false')
  await chip.click()
  await expect(chip).toHaveAttribute('aria-pressed', 'true')
})
