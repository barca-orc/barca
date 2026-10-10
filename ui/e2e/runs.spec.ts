import { expect, test, type APIRequestContext } from '@playwright/test'
import { seedRuns } from './helpers/seed'

const API = process.env.BARCA_E2E_API ?? 'http://127.0.0.1:8274'
type HistoryRun = { id: string; target: string | null; status: string }

async function history(request: APIRequestContext): Promise<HistoryRun[]> {
  const response = await request.get(`${API}/runs`)
  expect(response.ok()).toBeTruthy()
  return (await response.json()).runs
}

async function finishedRun(request: APIRequestContext, target: string, verb = 'run') {
  const before = new Set((await history(request)).map((run) => run.id))
  const response = await request.post(`${API}/${verb}/${target}`)
  expect(response.ok()).toBeTruthy()
  const { run_id } = await response.json()
  await expect.poll(async () => (await (await request.get(`${API}/status/${run_id}`)).json()).status,
    { timeout: 30_000 }).not.toMatch(/running|queued|pending/)
  const run = (await history(request)).find((entry) => !before.has(entry.id) && entry.target === target)
  expect(run, `durable history entry for ${target}`).toBeDefined()
  return run!
}

test.describe('run history', () => {
  test('lists real prior runs and opens durable logs and steps after reload', async ({ page, request }) => {
    const before = new Set((await history(request)).map((run) => run.id))
    await seedRuns(request, 'say_hello', 2)
    const seeded = (await history(request)).filter((run) => !before.has(run.id) && run.target === 'say_hello')
    expect(seeded).toHaveLength(2)

    await page.goto('/ui/#/runs')
    const runLink = page.getByRole('button', { name: seeded[0].id, exact: true })
    await expect(runLink).toBeVisible()
    await runLink.click()
    await expect(page).toHaveURL(new RegExp(`#/runs\\?run=${seeded[0].id}`))
    const details = page.getByRole('complementary', { name: 'Run details' })
    await expect(details).toContainText('say_hello')
    await expect(details).toContainText('success')
    await expect(details).toContainText('hello from e2e 6')
    await expect(details.getByRole('heading', { name: 'Steps', exact: true })).toBeVisible()
    await expect(details.locator('.barca-run-steps')).toContainText('say_hello')

    await page.reload()
    await expect(details).toContainText('hello from e2e 6')
    await expect(details).toContainText('success')
  })

  test('graph asset trigger exposes a live run and refreshes its inspectable details', async ({ page, request }) => {
    const before = new Set((await history(request)).map((run) => run.id))
    await page.goto('/ui/#/graph')
    await page.locator('.react-flow__node[data-id="pipeline.py:history_asset"]').click()
    const started = page.waitForResponse((response) => response.request().method() === 'POST'
      && decodeURIComponent(response.url()).endsWith('/get/pipeline.py:history_asset'))
    await page.getByRole('banner').getByRole('button', { name: 'Run', exact: true }).click()
    expect((await started).ok()).toBeTruthy()
    await expect(page).toHaveURL(/#\/assets\?.*view=graph/)
    await page.getByRole('link', { name: 'View run', exact: true }).click()
    await expect(page).toHaveURL(/#\/runs\?run=/)
    const details = page.getByRole('complementary', { name: 'Run details' })
    await expect(details).toContainText('history_asset')
    await expect(details).toContainText(/queued|running/)
    await expect(details).toContainText('history asset started', { timeout: 30_000 })
    await expect(details).toContainText('history asset finished', { timeout: 30_000 })
    await expect(details).toContainText('success')
    await expect(details.getByRole('heading', { name: 'Steps', exact: true })).toBeVisible()
    await expect(details.locator('.barca-run-steps')).toContainText('history_asset')

    const created = (await history(request)).find((run) => !before.has(run.id) && run.target === 'pipeline.py:history_asset')
    expect(created).toBeDefined()
    await page.goto('/ui/#/runs')
    await expect(page.getByRole('button', { name: created!.id, exact: true })).toBeVisible()
    await page.goto(`/ui/#/runs?run=${created!.id}`)
    await page.reload()
    await expect(details).toContainText('history asset finished')
    await expect(details).toContainText('success')
  })

  test('a cached asset trigger still creates a durable inspectable run', async ({ page, request }) => {
    await finishedRun(request, 'pipeline.py:numbers', 'get')
    const before = new Set((await history(request)).map((run) => run.id))
    await page.goto('/ui/#/graph')
    await page.locator('.react-flow__node[data-id="pipeline.py:numbers"]').click()
    const started = page.waitForResponse((response) => response.request().method() === 'POST'
      && decodeURIComponent(response.url()).endsWith('/get/pipeline.py:numbers'))
    await page.getByRole('banner').getByRole('button', { name: 'Run', exact: true }).click()
    expect((await started).ok()).toBeTruthy()
    await page.getByRole('link', { name: 'View run', exact: true }).click()
    const details = page.getByRole('complementary', { name: 'Run details' })
    await expect(details).toContainText('success')
    await expect(details.locator('.barca-run-steps')).toContainText('pipeline.py:numbers')
    await expect(details.locator('.barca-run-steps')).toContainText('cached')
    await expect(details.locator('.barca-kv').filter({ hasText: /^executed0$/ })).toHaveCount(1)
    await expect(details.locator('.barca-kv').filter({ hasText: /^cached1$/ })).toHaveCount(1)
    const created = (await history(request)).find((run) => !before.has(run.id) && run.target === 'pipeline.py:numbers')
    expect(created).toBeDefined()
    await expect(page.getByRole('button', { name: created!.id, exact: true })).toBeVisible()
    await page.goto(`/ui/#/runs?run=${created!.id}`)
    await page.reload()
    await expect(details.locator('.barca-run-steps')).toContainText('cached')
    await expect(details).toContainText('success')
    await expect(details.locator('.barca-kv').filter({ hasText: /^executed0$/ })).toHaveCount(1)
    await expect(details.locator('.barca-kv').filter({ hasText: /^cached1$/ })).toHaveCount(1)
    await page.screenshot({ path: '/tmp/barca-ui-runs-details.png', fullPage: true })
  })

  test('failed runs retain their error and logged output', async ({ page, request }) => {
    const run = await finishedRun(request, 'history_failure')
    expect(run.status).toBe('failed')
    await page.goto(`/ui/#/runs?run=${run.id}`)
    const details = page.getByRole('complementary', { name: 'Run details' })
    await expect(details).toContainText('failed')
    await expect(details).toContainText('history failure detail')
    await expect(details).toContainText('history failure logged')
    await page.reload()
    await expect(details).toContainText('history failure detail')
    await expect(details).toContainText('history failure logged')
  })
})
