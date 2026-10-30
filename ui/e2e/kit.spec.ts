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
    'HealthBar',
    'ConnectionBadge',
    'SearchInput / Select / Chip',
    'Skeleton',
    'SidePanel / Section / KeyValue',
    'DagNode · resting states',
    'GraphCanvas',
    'NodeInspector',
    'ResetDatabaseDialog',
    'Runs page',
    'Run details panel',
    'Tokens page',
  ]) {
    await expect(page.getByRole('heading', { name: title, exact: true })).toBeVisible()
  }
  // A chip is a toggle: pressing it flips aria-pressed.
  const chip = page.getByRole('button', { name: /assets/ })
  await expect(chip).toHaveAttribute('aria-pressed', 'false')
  await chip.click()
  await expect(chip).toHaveAttribute('aria-pressed', 'true')
})

// A page written as a program renders from a fixture and lists the messages it sends; no
// program runs, so nothing changes but the list.
test('kit page shows the messages a page view sends', async ({ page }) => {
  await page.goto('/ui/#/kit')
  const runs = page.locator('section', { has: page.getByRole('heading', { name: 'Runs page', exact: true }) })
  await runs.getByRole('button', { name: 'truncated', exact: true }).click()
  await runs.getByRole('button', { name: 'Load more' }).click()
  await expect(runs.getByLabel('Messages sent')).toContainText('{"type":"load_more"}')
})
