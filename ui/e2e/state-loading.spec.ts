import { expect, test } from '@playwright/test'
import { served } from './helpers/served'

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
const states = assets.map(asset => served({
  ...asset, name: asset.id.split(':')[1], partitioned: false, shape: null,
  durations: null, next_run: null, running: null, last_materialization: null,
  cache: { state: 'cached', reason: 'materialized', detail: 'Cached result' },
}))

// `GET /state/rollup?files=forecast.py` for these rows, as the server sums them: the group's two
// assets, and the file of the one in no group.
const ok = (n: number) => ({ failing: 0, blocked: 0, suspect: 0, behind: 0, undecided: 0, never_run: 0, ok: n })
const entry = (id: string, kind: string, label: string, parent: string | null, children: string[], n: number) =>
  ({ id, kind, label, parent, children, nodes: n, assets: n, running: 0, health: ok(n), asset_health: ok(n), data_coverage: { assets: n, measured: 0, suspect: 0 } })
const rollup = {
  as_of: '2026-11-17T14:57:00Z',
  entries: [
    entry('root', 'root', 'all', null, ['group:forecast.py:training', 'file:forecast.py'], 3),
    entry('group:forecast.py:training', 'group', 'Forecast training', 'root', ['forecast.py:model', 'forecast.py:rows'], 2),
    entry('forecast.py:model', 'node', 'model', 'group:forecast.py:training', [], 1),
    entry('forecast.py:rows', 'node', 'rows', 'group:forecast.py:training', [], 1),
    entry('file:forecast.py', 'file', 'forecast.py', 'root', ['forecast.py:other'], 1),
    entry('forecast.py:other', 'node', 'other', 'file:forecast.py', [], 1),
  ],
  roots: [],
  data_coverage: { assets: 3, measured: 0, suspect: 0 },
}

test('an empty pipeline selection renders an empty result; only the sidebar asks for the whole project', async ({ page }) => {
  const requests: string[] = []
  await page.route('**/assets', route => route.fulfill({ json: assets }))
  await page.route('**/groups', route => route.fulfill({ json: groups }))
  await page.route(/\/state(?:\?.*)?$/, route => {
    requests.push(route.request().url())
    return route.fulfill({ json: states })
  })
  await page.goto('/ui/#/assets?pipeline=missing.py')
  await expect(page.getByText('No assets in missing.py', { exact: true })).toBeVisible()
  expect(requests.every(url => !url.includes('?'))).toBe(true)
})

test('a status failure leaves grouped metadata and sidebar counts visible', async ({ page }) => {
  await page.route('**/assets', route => route.fulfill({ json: assets }))
  await page.route('**/groups', route => route.fulfill({ json: groups }))
  await page.route(/\/state(?:\?.*)?$/, route => route.fulfill({
    status: 500, json: { error: 'History is unavailable', remediation: 'Check the metadata volume' },
  }))
  await page.goto('/ui/#/assets?pipeline=forecast.py')
  await expect(page.locator('.barca-group-table tbody tr')).toHaveCount(2)
  const problem = page.getByRole('status').filter({ hasText: 'History is unavailable' })
  await expect(problem).toContainText('Check the metadata volume')
  await expect(page.locator('.barca-sidebar').getByRole('button', { name: 'other.py 1' })).toBeVisible()
})

test('a sidebar dot waits for its whole file: no dot says "fine" from part of it', async ({ page }) => {
  await page.route('**/assets', route => route.fulfill({ json: assets }))
  await page.route('**/groups', route => route.fulfill({ json: groups }))
  await page.route(/\/state(?:\?.*)?$/, route => {
    // The whole-project request fails: only the open pipeline is known.
    if (!route.request().url().includes('?')) return route.fulfill({ status: 500, json: { error: 'busy' } })
    return route.fulfill({ json: states.filter(n => n.id.startsWith('forecast.py:')) })
  })
  await page.goto('/ui/#/assets?pipeline=forecast.py')
  const sidebar = page.locator('.barca-sidebar')
  // The file's most urgent state, in the CLI's word, and what its count counts.
  await expect(sidebar.getByRole('button', { name: 'forecast.py 3' })).toHaveAttribute('title', 'forecast.py · 3 assets · fresh')
  await expect(sidebar.getByRole('button', { name: 'other.py 1' }).locator('.barca-pipe-nodot')).toHaveCount(1)
})

test('Reset database appears only for a database barca cannot use', async ({ page }) => {
  await page.route('**/assets', route => route.fulfill({ json: assets }))
  await page.route('**/groups', route => route.fulfill({ json: [] }))
  let unusable = false
  await page.route(/\/state(?:\?.*)?$/, route => unusable
    ? route.fulfill({ status: 503, json: { error: 'unreadable', reason: 'metadata_unusable', remediation: 'Restore a backup' } })
    : route.fulfill({ json: states }))
  await page.goto('/ui/#/assets')
  await expect(page.locator('.barca-table tbody tr:not(.barca-file-head)')).toHaveCount(4)
  await expect(page.getByRole('button', { name: 'Reset database' })).toHaveCount(0)
  unusable = true
  await page.reload()
  await expect(page.getByRole('button', { name: 'Reset database' })).toBeVisible()
})

for (const view of ['list', 'graph']) {
  test(`${view}: metadata renders before status, with stable layout and scoped requests`, async ({ page }) => {
    let release!: () => void
    const held = new Promise<void>(resolve => { release = resolve })
    const requests: URL[] = []
    await page.route('**/assets', route => route.fulfill({ json: assets }))
    await page.route('**/groups', route => route.fulfill({ json: groups }))
    await page.route(/\/state\/rollup(?:\?.*)?$/, route => route.fulfill({ json: rollup }))
    await page.route(/\/state(?:\?.*)?$/, async route => {
      const url = new URL(route.request().url())
      requests.push(url)
      await held
      // The server expands a group id to its members and a file to its nodes.
      const targets = url.searchParams.get('targets')?.split(',')
        .flatMap(t => groups.find(g => g.id === t)?.members ?? [t])
      const files = url.searchParams.get('files')?.split(',')
      await route.fulfill({
        json: targets ? states.filter(n => targets.includes(n.id))
          : files ? states.filter(n => files.some(f => n.id.startsWith(`${f}:`)))
          : states,
      })
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
      // A pipeline is asked for by its file: one short URL however many nodes it has.
      // The other request is the sidebar's whole-project health.
      expect(requests.every(url => url.search === '' || url.searchParams.get('files') === 'forecast.py')).toBe(true)
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
      // A group by its id, which the server expands.
      await expect.poll(() => requests.at(-1)?.searchParams.get('targets'))
        .toBe('group:forecast.py:training')
      await sidebar.getByRole('button', { name: 'All pipelines 4' }).click()
      await expect.poll(() => requests.at(-1)?.search).toBe('')
    } finally {
      release()
    }
  })
}
