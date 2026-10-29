import { expect, test } from '@playwright/test'

const assets = [
  { id: 'forecast.py:rows', kind: 'asset', inputs: [], freshness: 'always', env: [] },
  { id: 'forecast.py:model', kind: 'asset', inputs: ['forecast.py:rows'], freshness: 'always', env: [] },
  { id: 'forecast.py:other', kind: 'asset', inputs: [], freshness: 'always', env: [] },
  { id: 'other.py:standalone', kind: 'asset', inputs: [], freshness: 'always', env: [] },
]
const groups = [{
  id: 'group:forecast.py:training', name: 'Forecast training', description: '',
  members: ['forecast.py:rows', 'forecast.py:model'], output: 'forecast.py:model',
}]
const states = assets.map(asset => ({
  ...asset, name: asset.id.split(':')[1], partitioned: false, shape: null,
  durations: null, next_run: null, running: null, last_materialization: null,
  cache: { state: 'cached', reason: 'materialized', detail: 'Cached result' },
}))

test('an empty pipeline selection renders an empty result without querying the whole project', async ({ page }) => {
  const requests: string[] = []
  await page.route('**/assets', route => route.fulfill({ json: assets }))
  await page.route('**/groups', route => route.fulfill({ json: groups }))
  await page.route(/\/state(?:\?.*)?$/, route => {
    requests.push(route.request().url())
    return route.fulfill({ json: states })
  })
  await page.goto('/ui/#/assets?pipeline=missing.py')
  await expect(page.getByText('Nothing matches.', { exact: true })).toBeVisible()
  expect(requests).toEqual([])
})

test('a status failure leaves grouped metadata and sidebar counts visible', async ({ page }) => {
  await page.route('**/assets', route => route.fulfill({ json: assets }))
  await page.route('**/groups', route => route.fulfill({ json: groups }))
  await page.route(/\/state(?:\?.*)?$/, route => route.fulfill({
    status: 500, json: { error: 'History is unavailable', remediation: 'Check the metadata volume' },
  }))
  await page.goto('/ui/#/assets?pipeline=forecast.py')
  await expect(page.locator('.barca-group-table tbody tr')).toHaveCount(2)
  await expect(page.getByRole('status')).toContainText('History is unavailable')
  await expect(page.getByRole('status')).toContainText('Check the metadata volume')
  await expect(page.locator('.barca-sidebar').getByRole('button', { name: 'other.py 1' })).toBeVisible()
})

for (const view of ['list', 'graph']) {
  test(`${view}: metadata renders before status, with stable layout and scoped requests`, async ({ page }) => {
    let release!: () => void
    const held = new Promise<void>(resolve => { release = resolve })
    const requests: URL[] = []
    await page.route('**/assets', route => route.fulfill({ json: assets }))
    await page.route('**/groups', route => route.fulfill({ json: groups }))
    await page.route(/\/state(?:\?.*)?$/, async route => {
      const url = new URL(route.request().url())
      requests.push(url)
      await held
      const targets = url.searchParams.get('targets')?.split(',')
      await route.fulfill({ json: targets ? states.filter(n => targets.includes(n.id)) : states })
    })
    try {
      await page.goto(`/ui/#/assets?pipeline=forecast.py${view === 'graph' ? '&view=graph' : ''}`)
      const entries = view === 'graph'
        ? page.locator('.react-flow__node')
        : page.locator('.barca-group-table tbody tr')
      await expect(entries).toHaveCount(2)
      const sidebar = page.locator('.barca-sidebar')
      await expect(sidebar.getByRole('button', { name: 'forecast.py 3' })).toBeVisible()
      await expect(sidebar.getByRole('button', { name: 'other.py 1' })).toBeVisible()
      await expect.poll(() => requests.length).toBeGreaterThan(0)
      expect(requests.every(url => url.searchParams.has('targets'))).toBe(true)
      expect(new Set(requests.at(-1)!.searchParams.get('targets')!.split(',')))
        .toEqual(new Set(assets.slice(0, 3).map(n => n.id)))
      await expect(page.getByText('Checking…', { exact: true }).first()).toBeVisible()
      // Let the initial graph fit settle before comparing geometry across status arrival.
      if (view === 'graph') await page.waitForTimeout(400)
      const before = await entries.evaluateAll(nodes => nodes.map(node => {
        const { x, y, width, height } = node.getBoundingClientRect()
        return { x, y, width, height }
      }))
      release()
      await expect(page.getByText('Checking…', { exact: true })).toHaveCount(0)
      await expect(entries).toHaveCount(2)
      const after = await entries.evaluateAll(nodes => nodes.map(node => {
        const { x, y, width, height } = node.getBoundingClientRect()
        return { x, y, width, height }
      }))
      expect(after).toEqual(before)
      const group = view === 'graph'
        ? page.locator('.react-flow__node[data-id="group:forecast.py:training"]')
        : page.locator('tr[data-group="group:forecast.py:training"]')
      await group.dblclick()
      await expect.poll(() => requests.at(-1)?.searchParams.get('targets')?.split(',').sort())
        .toEqual(['forecast.py:model', 'forecast.py:rows'])
      await sidebar.getByRole('button', { name: 'All pipelines 4' }).click()
      await expect.poll(() => requests.at(-1)?.searchParams.has('targets')).toBe(false)
    } finally {
      release()
    }
  })
}
