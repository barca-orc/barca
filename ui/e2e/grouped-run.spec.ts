import { expect, test } from '@playwright/test'
import { isStartRun, waitForRun } from './helpers/seed'

// Group metadata changes the UI route; the state, run admission, SSE and history are real.
const group = {
  id: 'group:pipeline.py:work', name: 'Grouped work', description: '',
  members: ['pipeline.py:numbers', 'pipeline.py:say_hello', 'pipeline.py:history_clock', 'pipeline.py:history_asset'],
  output: 'pipeline.py:history_asset',
}

test.beforeEach(async ({ page }) => {
  await page.route('**/groups', route => route.fulfill({ json: [group] }))
  // The server does not know the mocked group, so stand in for its expansion of the group id:
  // the state itself stays real.
  await page.route(/\/state\?/, route => {
    const url = new URL(route.request().url())
    if (url.searchParams.get('targets') !== group.id) return route.continue()
    url.searchParams.set('targets', group.members.join(','))
    return route.continue({ url: url.toString() })
  })
})

for (const view of ['list', 'graph']) {
  for (const [target, verb] of [['history_asset', 'get'], ['history_clock', 'get'], ['say_hello', 'run']] as const) {
    test(`${view}: runs the grouped ${target} with ${verb}`, async ({ page, request }) => {
      const nodeId = `pipeline.py:${target}`
      const selection = view === 'graph' ? `view=graph&focus=${nodeId}` : `node=${nodeId}`
      await page.goto(`/ui/#/assets?pipeline=pipeline.py&group=${group.id}&${selection}`)
      await expect(page.getByRole('navigation', { name: 'Group breadcrumb' })).toContainText('Grouped work')
      const panel = page.getByRole('complementary', { name: `${target} details` })
      const topbar = page.getByRole('banner').getByRole('button', { name: `Run ${target}`, exact: true })
      await expect(topbar).toBeEnabled()
      await expect(panel.getByRole('button', { name: 'Run', exact: true })).toBeEnabled()
      const started = page.waitForResponse(response => isStartRun(response.request(), verb, nodeId))
      await (view === 'graph' ? topbar : panel.getByRole('button', { name: 'Run', exact: true })).click()
      const response = await started
      expect(response.status()).toBe(202)
      const handle = (await response.json()).run_id
      await page.getByRole('link', { name: 'View run', exact: true }).click()
      await expect(page).toHaveURL(new RegExp(`#/runs\\?run=${handle}`))
      const details = page.getByRole('complementary', { name: 'Run details' })
      await expect(details).toContainText(target)
      await waitForRun(request, handle)
      await expect(details).toContainText('success', { timeout: 30_000 })
      if (verb === 'run') await expect(details).toContainText('hello from e2e 6')
    })
  }

  test(`${view}: read-only mode disables both run actions`, async ({ page }) => {
    await page.route('**/health', async route => {
      const response = await route.fetch()
      await route.fulfill({ json: { ...await response.json(), read_only: true } })
    })
    const selection = view === 'graph' ? 'view=graph&focus=pipeline.py:numbers' : 'node=pipeline.py:numbers'
    await page.goto(`/ui/#/assets?pipeline=pipeline.py&group=${group.id}&${selection}`)
    const topbar = page.getByRole('banner').getByRole('button', { name: 'Run numbers', exact: true })
    const panel = page.getByRole('complementary', { name: 'numbers details' })
    await expect(topbar).toBeDisabled()
    await expect(topbar).toHaveAttribute('title', 'This server is read-only')
    await expect(panel.getByRole('button', { name: 'Run', exact: true })).toBeDisabled()
    await expect(panel.getByRole('button', { name: 'Run', exact: true })).toHaveAttribute('title', 'This server is read-only')
  })
}

test('the grouped list topbar shows running before the first stream event arrives', async ({ page }) => {
  let release!: () => void
  const held = new Promise<void>(resolve => { release = resolve })
  await page.route(/\/runs\/[^/]+\/events$/, async route => {
    await held
    await route.continue()
  })
  try {
    await page.goto(`/ui/#/assets?pipeline=pipeline.py&group=${group.id}&node=pipeline.py:history_asset`)
    const started = page.waitForResponse(response => isStartRun(response.request(), 'get', 'pipeline.py:history_asset'))
    await page.getByRole('banner').getByRole('button', { name: 'Run history_asset', exact: true }).click()
    expect((await started).status()).toBe(202)
    const row = page.getByRole('row').filter({ hasText: 'history_asset' })
    await expect(row).toContainText('Running')
    await expect(page.getByRole('complementary', { name: 'history_asset details' })).toContainText('Running')
    // The breadcrumb's root is the open pipeline.
    await page.getByRole('navigation', { name: 'Group breadcrumb' })
      .getByRole('button', { name: 'pipeline.py', exact: true }).click()
    await expect(page.locator(`tr[data-group="${group.id}"]`)).toContainText('1 running')
  } finally {
    release()
  }
})

test('a refused grouped run shows its error in the node panel', async ({ page }) => {
  await page.route('**/runs', route => route.request().method() === 'POST'
    ? route.fulfill({ status: 503, json: { error: 'Run queue is unavailable' } })
    : route.continue())
  await page.goto(`/ui/#/assets?pipeline=pipeline.py&group=${group.id}&node=pipeline.py:numbers`)
  const panel = page.getByRole('complementary', { name: 'numbers details' })
  await panel.getByRole('button', { name: 'Run', exact: true }).click()
  await expect(panel.getByRole('alert')).toContainText('Run queue is unavailable')
})

test('the ungrouped list node panel uses the same run action', async ({ page, request }) => {
  await page.route('**/groups', route => route.fulfill({ json: [] }))
  await page.goto('/ui/#/tasks?node=pipeline.py:say_hello')
  const panel = page.getByRole('complementary', { name: 'say_hello details' })
  const started = page.waitForResponse(response => isStartRun(response.request(), 'run', 'pipeline.py:say_hello'))
  await panel.getByRole('button', { name: 'Run', exact: true }).click()
  const handle = (await (await started).json()).run_id
  await waitForRun(request, handle)
  await page.getByRole('link', { name: 'View run', exact: true }).click()
  await expect(page.getByRole('complementary', { name: 'Run details' })).toContainText('hello from e2e 6')
})
