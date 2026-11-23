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
