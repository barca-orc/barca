import { test, expect } from '@playwright/test'

test('incident controls put replacement data beside containment', async ({ page }) => {
  await page.route(/\/assets\/[^/]+\/versions/, route => route.fulfill({ json: { node_id: 'pipeline.py:numbers', pinnable: true, pin: null, versions: [] } }))
  await page.goto('/ui/#/assets?node=pipeline.py%3Anumbers')
  const panel = page.getByRole('complementary', { name: 'numbers details' })
  await panel.getByRole('tab', { name: 'Incident', exact: true }).click()
  await expect(panel.getByRole('button', { name: 'Upload replacement…' })).toBeVisible()
  await expect(panel.getByRole('button', { name: 'Pause this asset and downstream' })).toBeVisible()
})


test('resume a pipeline preserves pauses in other files', async ({ page, request }) => {
  await request.post('http://127.0.0.1:8274/pauses', { data: { target: 'other.py' } })
  try {
    await page.goto('/ui/#/assets?node=pipeline.py%3Anumbers')
    const panel = page.getByRole('complementary', { name: 'numbers details' })
    await panel.getByRole('tab', { name: 'Incident', exact: true }).click()
    await panel.getByRole('button', { name: 'Pause pipeline (file)' }).click()
    await page.getByRole('dialog').getByRole('button', { name: 'Pause pipeline', exact: true }).click()
    await expect(panel.getByRole('button', { name: 'Resume pipeline (file)' })).toBeVisible()
    await expect(panel.getByRole('button', { name: 'Pause pipeline (file)' })).toHaveCount(0)
    await panel.getByRole('button', { name: 'Resume pipeline (file)' }).click()
    await expect(page.getByRole('dialog')).toContainText('pipeline.py')
    await page.getByRole('dialog').getByRole('button', { name: 'Resume pipeline', exact: true }).click()
    await expect(panel.getByRole('button', { name: 'Pause pipeline (file)' })).toBeVisible()
    const paused = await (await request.get('http://127.0.0.1:8274/pauses')).json()
    expect(paused.paused.map((p: { id: string }) => p.id)).toEqual(['other.py:history_asset'])
  } finally {
    await request.post('http://127.0.0.1:8274/pauses/resume', { data: { all: true } })
  }
})

test('cancel a queued run confirms, reports an error, and allows retry', async ({ page, request }) => {
  const api = 'http://127.0.0.1:8274'
  await request.post(`${api}/queue/pause`, { data: {} })
  let id = ''
  try {
    const started = await request.post(`${api}/runs`, { data: { command: 'get', targets: ['numbers'] } })
    expect(started.ok()).toBeTruthy()
    id = (await started.json()).run_id
    let deletes = 0
    await page.route(`**/runs/${id}`, route => {
      if (route.request().method() !== 'DELETE') return route.fallback()
      deletes++
      return deletes === 1 ? route.fulfill({ status: 503, json: { error: 'temporary cancellation failure' } }) : route.fallback()
    })
    await page.goto(`/ui/#/runs?run=${id}`)
    const panel = page.getByRole('complementary', { name: 'Run details' })
    await panel.getByRole('button', { name: 'Cancel run', exact: true }).click()
    await expect(page.getByRole('dialog')).toContainText('does not start')
    await page.getByRole('dialog').getByRole('button', { name: 'Cancel', exact: true }).click()
    expect(deletes).toBe(0)
    await panel.getByRole('button', { name: 'Cancel run', exact: true }).click()
    await page.getByRole('dialog').getByRole('button', { name: 'Cancel run', exact: true }).click()
    await expect(panel.getByRole('alert')).toContainText('Could not cancel')
    await panel.getByRole('button', { name: 'Cancel run', exact: true }).click()
    await page.getByRole('dialog').getByRole('button', { name: 'Cancel run', exact: true }).click()
    await expect(panel).toContainText('cancelled')
    await expect(panel.getByRole('button', { name: 'Cancel run', exact: true })).toHaveCount(0)
    expect((await (await request.get(`${api}/runs/${id}`)).json()).status).toBe('cancelled')
  } finally {
    if (id) await request.delete(`${api}/runs/${id}`)
    await request.post(`${api}/queue/resume`, { data: {} })
  }
})
