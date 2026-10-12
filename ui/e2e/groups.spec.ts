import { readFileSync } from 'node:fs'
import { expect, test } from '@playwright/test'

// Derive the demo's node names from its explicit Python definitions so this
// UI spike can be tested against the usual six-node e2e server as well.
const source = readFileSync(
  new URL('../demo/modeling.py', import.meta.url),
  'utf8',
)
const names = [
  ...source.matchAll(
    /^def ((?:validate__)?(?:data|cv|train|test|release)__\w+)\(/gm,
  ),
].map((match) => match[1])
const assets = names.map((name) => ({
  id: `modeling.py:${name}`,
  kind: name.startsWith('validate__') ? 'task' : 'asset',
  inputs: name.startsWith('validate__')
    ? [`modeling.py:${name.replace(/^validate__/, '')}`]
    : [],
  freshness: { type: 'Always' },
  env: [],
}))

test.beforeEach(async ({ page }) => {
  await page.route('**/assets', (route) => route.fulfill({ json: assets }))
  await page.route('**/state', (route) =>
    route.fulfill({
      json: assets.map((asset) => ({
        ...asset,
        name: asset.id.split(':')[1],
        partitioned: false,
        shape: null,
        durations: null,
        next_run: null,
        cache: {
          state: asset.kind === 'task' ? 'always_runs' : 'cached',
          reason: asset.kind === 'task' ? 'task' : 'materialized',
          detail: 'Demo state',
        },
        last_materialization: {
          status: 'success',
          created_at: '2026-10-09 00:00:00',
          elapsed_seconds: null,
          run_hash: null,
          artifact: null,
          format: null,
          size_bytes: null,
        },
      })),
    }),
  )
})

test('double-click navigates nested groups and List keeps the current scope after reload', async ({
  page,
}) => {
  await page.goto('/ui/#/assets?pipeline=modeling.py&view=graph')
  await expect(page.locator('.react-flow__node')).toHaveCount(5)
  await page
    .locator('.react-flow__node[data-id="group:cross_validation"]')
    .dblclick()
  await expect(page.locator('.react-flow__node')).toHaveCount(6)
  await page.locator('.react-flow__node[data-id="group:fold_03"]').dblclick()
  await expect(page.locator('.react-flow__node')).toHaveCount(3)
  await page
    .locator('.react-flow__node[data-id="group:fold_03/fitting"]')
    .focus()
  await page.keyboard.press('Enter')
  await expect(page.locator('.react-flow__node')).toHaveCount(6)
  await expect(
    page.getByRole('navigation', { name: 'Group breadcrumb' }),
  ).toContainText('Cross-validation')
  await expect(
    page.getByRole('navigation', { name: 'Group breadcrumb' }),
  ).toContainText('Fold 3')
  await page.getByRole('button', { name: 'List', exact: true }).click()
  await expect(page.locator('.barca-group-table tbody tr')).toHaveCount(6)
  await page.reload()
  await expect(
    page.getByRole('heading', { name: 'Model fitting', exact: true }),
  ).toBeVisible()
  await expect(page.locator('.barca-group-table tbody tr')).toHaveCount(6)
  await page.getByRole('button', { name: 'Up one level' }).click()
  await expect(page.locator('.barca-group-table tbody tr')).toHaveCount(3)
})

test('failed validation makes ancestors red while their output remains cached; navigation sends no run requests', async ({
  page,
}) => {
  const mutations: string[] = []
  page.on('request', (request) => {
    if (request.method() !== 'GET') mutations.push(request.url())
  })
  await page.goto('/ui/#/assets?pipeline=modeling.py&view=graph')
  const cv = page.locator('.react-flow__node[data-id="group:cross_validation"]')
  await expect(cv.locator('[data-status="success"]')).toBeVisible()
  await page
    .getByRole('button', { name: 'Preview failed check', exact: true })
    .click()
  await expect(cv.locator('[data-status="failed"]')).toBeVisible()
  await cv.click()
  const panel = page.getByRole('complementary', {
    name: 'Cross-validation group details',
  })
  await expect(panel).toContainText('1 failed member')
  await expect(panel.getByText('Cached', { exact: true })).toBeVisible()
  await expect(panel).toContainText('validate__cv__fold_03__trained_model')
  await panel
    .getByRole('button', { name: /validate__cv__fold_03__trained_model/ })
    .click()
  await expect(
    page.getByRole('navigation', { name: 'Group breadcrumb' }),
  ).toContainText('Model fitting')
  await expect(
    page.locator(
      '.react-flow__node[data-id="modeling.py:validate__cv__fold_03__trained_model"] [data-status="failed"]',
    ),
  ).toBeVisible()
  await expect(
    page.locator(
      '.react-flow__node[data-id="modeling.py:cv__fold_03__trained_model"] [data-status="success"]',
    ),
  ).toBeVisible()
  await page
    .getByRole('button', { name: 'Preview failed check', exact: true })
    .click()
  await expect(
    page.locator(
      '.react-flow__node[data-id="modeling.py:validate__cv__fold_03__trained_model"] [data-status="success"]',
    ),
  ).toBeVisible()
  expect(mutations).toEqual([])
})

test('table groups support search, arrow selection and Enter to open', async ({
  page,
}) => {
  await page.goto('/ui/#/assets?pipeline=modeling.py')
  const rows = page.locator('.barca-group-table tbody tr')
  await expect(rows).toHaveCount(5)
  await rows.first().click()
  await expect(rows.first()).toHaveAttribute('aria-selected', 'true')
  await page.keyboard.press('ArrowDown')
  await expect(rows.nth(1)).toHaveAttribute('aria-selected', 'true')
  await page.keyboard.press('Enter')
  await expect(rows).toHaveCount(6)
  await page.getByRole('button', { name: 'Modeling', exact: true }).click()
  await expect(rows).toHaveCount(5)
  await page
    .getByPlaceholder('Search groups and members')
    .fill('fold_03__trained_model')
  await expect(rows).toHaveCount(1)
  await expect(rows.first()).toContainText('Cross-validation')
})

test('flat comparison exposes all 152 original nodes and group mode restores five cards', async ({
  page,
}) => {
  await page.goto('/ui/#/assets?pipeline=modeling.py&view=graph')
  await expect(page.locator('.react-flow__node')).toHaveCount(5)
  await page.getByRole('button', { name: 'All nodes', exact: true }).click()
  await expect(page.locator('.react-flow__node')).toHaveCount(152)
  await page.getByRole('button', { name: 'Groups', exact: true }).click()
  await expect(page.locator('.react-flow__node')).toHaveCount(5)
})
