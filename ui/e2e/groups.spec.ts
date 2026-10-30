const metadata = JSON.parse(readFileSync(new URL('./group-metadata.json', import.meta.url), 'utf8'))
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
  freshness: 'always',
  env: [],
}))

test.beforeEach(async ({ page }) => {
  await page.route('**/groups', route => route.fulfill({ json: metadata }))
  await page.route('**/assets', (route) => route.fulfill({ json: assets }))
  await page.route(/\/state(?:\?.*)?$/, (route) =>
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
    .locator('.react-flow__node[data-id="group:modeling.py:cross_validation"]')
    .dblclick()
  await expect(page.locator('.react-flow__node')).toHaveCount(6)
  await page.locator('.react-flow__node[data-id="group:modeling.py:fold_03"]').dblclick()
  await expect(page.locator('.react-flow__node')).toHaveCount(3)
  // React Flow keeps a node hidden, so not focusable, until it is measured.
  const fitting = page.locator('.react-flow__node[data-id="group:modeling.py:fold_03__fitting"]')
  await expect(async () => {
    await fitting.focus()
    await expect(fitting).toBeFocused({ timeout: 200 })
  }).toPass()
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
  const cv = page.locator('.react-flow__node[data-id="group:modeling.py:cross_validation"]')
  await expect(cv.locator('[data-status="success"]')).toBeVisible()
  await page
    .getByRole('button', { name: 'Preview failed check', exact: true })
    .click()
  await expect(cv.locator('[data-status="failed"]')).toBeVisible()
  await cv.click()
  const panel = page.getByRole('complementary', {
    name: 'Cross-validation group details',
  })
  await expect(panel).toContainText('1 failed')
  await expect(panel.getByText('Healthy', { exact: true })).toBeVisible()
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
  await expect(rows).toHaveCount(2)
  await page.getByRole('button', { name: 'Modeling', exact: true }).click()
  await expect(rows).toHaveCount(5)
  await page
    .getByPlaceholder('Search groups and members')
    .fill('fold_03__trained_model')
  // The group that holds the matches, then every matching node in it, with its path.
  await expect(rows).toHaveCount(3)
  await expect(rows.first()).toContainText('Cross-validation')
  await expect(rows.nth(1)).toContainText('in Cross-validation › Fold 3')
  await rows.nth(1).click()
  await expect(page.getByRole('navigation', { name: 'Group breadcrumb' })).toContainText('Fold 3')
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

test('a different Python pipeline uses server groups and keeps ungrouped nodes visible', async ({ page }) => {
  const nodes = [
    { id: 'forecast.py:rows', kind: 'asset', inputs: [], freshness: 'always', env: [] },
    { id: 'forecast.py:model', kind: 'asset', inputs: ['forecast.py:rows'], freshness: 'always', env: [] },
    { id: 'forecast.py:other', kind: 'asset', inputs: [], freshness: 'always', env: [] },
  ]
  await page.route('**/assets', route => route.fulfill({ json: nodes }))
  await page.route('**/groups', route => route.fulfill({ json: [
    { id: 'group:forecast.py:training', name: 'Forecast training', description: '', members: ['forecast.py:rows', 'forecast.py:model'], output: 'forecast.py:model' },
  ] }))
  await page.route(/\/state(?:\?.*)?$/, route => route.fulfill({ json: [] }))
  await page.goto('/ui/#/assets?pipeline=forecast.py&view=graph')
  await expect(page.locator('.react-flow__node')).toHaveCount(2)
  await expect(page.getByRole('button', { name: 'Preview failed check' })).toHaveCount(0)
  await page.locator('.react-flow__node[data-id="group:forecast.py:training"]').dblclick()
  await expect(page.locator('.react-flow__node')).toHaveCount(2)
  await expect(page.getByRole('navigation', { name: 'Group breadcrumb' })).toContainText('Forecast training')
  await page.getByRole('button', { name: 'List', exact: true }).click()
  await expect(page.locator('.barca-group-table tbody tr')).toHaveCount(2)
})

test('groups open worst first, a sort is kept in the URL, and rows read by their distinct part', async ({ page }) => {
  await page.goto('/ui/#/assets?pipeline=modeling.py&preview=failed')
  const rows = page.locator('.barca-group-table tbody tr')
  await expect(rows.first()).toContainText('Cross-validation')
  await expect(page.getByRole('columnheader', { name: 'Failures' })).toBeVisible()
  await expect(page.getByRole('columnheader', { name: 'Output' })).toHaveCount(0)
  await page.getByLabel('Sort').selectOption('name')
  await expect(page).toHaveURL(/sort=name/)
  await page.goto('/ui/#/assets?pipeline=modeling.py&group=group:modeling.py:split')
  await expect(page.getByRole('columnheader', { name: 'Duration' })).toBeVisible()
  await expect(page.getByRole('columnheader', { name: 'Contents' })).toHaveCount(0)
})

test('failures are grouped by cause and a row can be muted', async ({ page }) => {
  await page.goto('/ui/#/assets?pipeline=modeling.py&preview=failed&show=failures')
  await expect(page.locator('.barca-failure-cause')).toHaveCount(1)
  await expect(page.locator('.barca-failure-cause')).toContainText('1 failed')
  await page.getByRole('button', { name: /^Mute / }).click()
  await expect(page.getByRole('button', { name: 'Muted · 1' })).toBeVisible()
  await expect(page.locator('.barca-failure-cause')).toHaveCount(0)
  await page.reload()
  await expect(page.getByRole('button', { name: 'Muted · 1' })).toBeVisible()
})

test('a failure row retries its node and opens its run', async ({ page }) => {
  const started: unknown[] = []
  await page.route('**/runs', (route) => {
    if (route.request().method() !== 'POST') return route.fallback()
    started.push(route.request().postDataJSON())
    return route.fulfill({ status: 202, json: { run_id: '41', status: 'running', position: null } })
  })
  await page.route('**/assets/modeling*', (route) => route.fulfill({
    json: { asset: {}, stats: { recent_runs: [{ status: 'failed', run_id: '41' }] } },
  }))
  await page.goto('/ui/#/assets?pipeline=modeling.py&preview=failed&show=failures')
  await page.getByRole('button', { name: /^Retry / }).click()
  await expect(page.getByRole('status').filter({ hasText: /^Retrying / })).toBeVisible()
  expect(started).toHaveLength(1)
  await page.getByRole('button', { name: /^Open run of / }).click()
  await expect(page).toHaveURL(/#\/runs\?run=41/)
})
