import { writeFile, readFile } from 'node:fs/promises'
import { fileURLToPath } from 'node:url'
import { expect, test } from '@playwright/test'
import { waitForRun } from './helpers/seed'
import type { RunView } from '../src/lib/types'

const API = process.env.BARCA_E2E_API ?? 'http://127.0.0.1:8274'

const fixture = fileURLToPath(new URL('./fixture/pipeline.py', import.meta.url))

let original: string

test.beforeAll(async ({ request }) => {
  original = await readFile(fixture, 'utf8')
  await writeFile(fixture, original + `\nfrom barca import Manual

@asset(freshness=Manual)
def model():
    return 42

@task(inputs={"value": model})
def publish_manual(value):
    print(value)
`)
  await expect.poll(async () => (await request.get(`${API}/assets`)).text()).toContain('pipeline.py:publish_manual')
})

test.afterAll(async () => { if (original !== undefined) await writeFile(fixture, original) })

for (const target of ['pipeline.py:say_hello', 'pipeline.py:publish_manual']) {
  test(`Run ${target} executes the task while retaining cached upstreams`, async ({ page, request }) => {
    // An explicit hard refresh establishes every input, including the Manual asset.
    const seed = await request.post(`${API}/runs`, { data: { command: 'run', targets: [target], refresh_all: true } })
    expect(seed.ok()).toBeTruthy()
    await waitForRun(request, (await seed.json()).run_id)
    await page.goto(`/ui/#/assets?node=${encodeURIComponent(target)}`)
    const panel = page.getByRole('complementary', { name: `${target.split(':')[1]} details` })
    const button = panel.getByRole('button', { name: 'Run', exact: true })
    await expect(button).toHaveAttribute('title', /using cached results/)
    for (let repeat = 0; repeat < 2; repeat++) {
      const started = page.waitForResponse(r => r.request().method() === 'POST' && r.url().endsWith('/runs'))
      await button.click()
      const response = await started
      expect(response.request().postDataJSON()).toEqual({ command: 'run', targets: [target] })
      const id = (await response.json()).run_id
      await waitForRun(request, id)
      const detail: RunView = await (await request.get(`${API}/runs/${id}`)).json()
      expect(detail.status).toBe('success')
      expect(detail.steps.filter(step => step.status === 'success').map(step => step.node_id)).toEqual([target])
      expect(detail.steps_cached).toBeGreaterThan(0)
      await expect(button).toBeEnabled()
    }
  })
}

test('Including Manual uses the cache and hard refresh asks before recomputing it', async ({ page, request }) => {
  const target = 'pipeline.py:publish_manual'
  const model = 'pipeline.py:model'
  const seed = await request.post(`${API}/runs`, { data: { command: 'run', targets: [target], refresh_all: true } })
  await waitForRun(request, (await seed.json()).run_id)
  await writeFile(fixture, (await readFile(fixture, 'utf8')).replace('return 42', `return ${Date.now()}`))
  await page.goto(`/ui/#/assets?node=${encodeURIComponent(target)}`)
  const panel = page.getByRole('complementary', { name: 'publish_manual details' })
  const manual = panel.getByRole('checkbox', { name: 'Including Manual' })
  await expect(manual).not.toBeChecked()
  await manual.check()
  const start = async (hard: boolean) => {
    if (hard) {
      await panel.getByRole('button', { name: 'Hard refresh…' }).click()
      const dialog = page.getByRole('dialog', { name: 'Hard refresh this scope?' })
      await expect(dialog).toContainText(model)
      await dialog.getByRole('button', { name: 'Cancel', exact: true }).click()
      await panel.getByRole('button', { name: 'Hard refresh…' }).click()
    }
    const response = page.waitForResponse(r => r.request().method() === 'POST' && r.url().endsWith('/runs'))
    if (hard) await page.getByRole('dialog').getByRole('button', { name: 'Hard refresh', exact: true }).click()
    else await panel.getByRole('button', { name: 'Run', exact: true }).click()
    const started = await response
    expect(started.request().postDataJSON()).toEqual({ command: 'run', targets: [target], ...(hard ? { refresh_all: true } : { include_manual: true }) })
    const id = (await started.json()).run_id
    await waitForRun(request, id)
    const run: RunView = await (await request.get(`${API}/runs/${id}`)).json()
    expect(run.status).toBe('success')
    await expect(manual).toBeEnabled()
    return run.steps.filter(step => step.status === 'success').map(step => step.node_id)
  }
  expect(await start(false)).toContain(model)
  expect(await start(false)).not.toContain(model)
  expect(await start(true)).toContain(model)
  await request.post(`${API}/pauses`, { data: { target: model } })
  try {
    expect(await start(true)).not.toContain(model)
  } finally {
    await request.post(`${API}/pauses/resume`, { data: { target: model } })
  }
})

test('pipeline options are explicit and Manual is unchecked by default', async ({ page, request }) => {
  const seed = await request.post(`${API}/runs`, { data: { command: 'get', targets: ['pipeline.py:model'], include_manual: true } })
  await waitForRun(request, (await seed.json()).run_id)
  await page.goto('/ui/#/assets?pipeline=pipeline.py')
  const toolbar = page.locator('.barca-view-head')
  const manual = toolbar.getByRole('checkbox', { name: 'Including Manual' })
  await expect(manual).not.toBeChecked()
  await manual.check()
  const started = page.waitForResponse(r => r.request().method() === 'POST' && r.url().endsWith('/runs'))
  await toolbar.getByRole('button', { name: /^Run pipeline/ }).click()
  const response = await started
  expect(response.request().postDataJSON()).toEqual({ command: 'get', targets: [], files: ['pipeline.py'], include_manual: true })
  await waitForRun(request, (await response.json()).run_id)
  await expect(toolbar.getByRole('button', { name: 'Hard refresh…' })).toBeEnabled()
  await toolbar.getByRole('button', { name: 'Hard refresh…' }).click()
  await expect(page.getByRole('dialog')).toContainText('pipeline.py:model')
  await page.getByRole('dialog').getByRole('button', { name: 'Cancel', exact: true }).click()
})
