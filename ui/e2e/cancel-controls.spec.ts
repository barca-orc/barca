import { expect, test } from '@playwright/test'
import { writeFile, rm } from 'node:fs/promises'
import { fileURLToPath } from 'node:url'

const API = process.env.BARCA_E2E_API ?? 'http://127.0.0.1:8274'
const hold = fileURLToPath(new URL('./fixture/.cancel-hold', import.meta.url))

for (const surface of ['run list', 'running queue', 'pending queue', 'badge', 'notice'] as const) {
  test(`cancels through the ${surface} with confirmation`, async ({ page, request }) => {
    await writeFile(hold, '')
    let id: string | undefined
    try {
      const pending = surface === 'pending queue' || surface === 'notice'
      if (pending) await request.post(`${API}/queue/pause`, { data: {} })
      if (surface === 'notice') {
        await page.goto('/ui/#/assets?pipeline=pipeline.py')
        const started = page.waitForResponse(r => r.request().method() === 'POST' && r.url().endsWith('/runs'))
        await page.locator('.barca-view-head').getByRole('button', { name: /^Run/ }).click()
        id = (await (await started).json()).run_id
      } else {
        const response = await request.post(`${API}/runs`, { data: { command: 'get', targets: ['pipeline.py:history_asset'], refresh_all: true } })
        expect(response.ok(), await response.text()).toBeTruthy()
        id = (await response.json()).run_id
        if (!pending) await expect.poll(async () => (await (await request.get(`${API}/runs/${id}`)).json()).status).toBe('running')
        await page.goto(surface === 'badge' ? '/ui/#/assets?node=pipeline.py%3Ahistory_asset' : '/ui/#/runs')
      }
      const region = surface === 'run list' ? page.locator('.barca-run-table tr').filter({ hasText: id })
        : surface === 'badge' ? page.locator('.barca-running-controls').filter({ has: page.locator(`a[href*="${id}"]`) }).first()
        : surface === 'notice' ? page.locator('.barca-run-notice')
        : page.locator('.barca-queue-runs tr').filter({ hasText: id })
      const cancel = region.getByRole('button', { name: 'Cancel run', exact: true })
      await expect(cancel).toBeEnabled()
      await cancel.click()
      const dialog = page.getByRole('dialog', { name: `Cancel run ${id}?` })
      await expect(dialog).toBeVisible()
      await dialog.getByRole('button', { name: 'Cancel', exact: true }).click()
      expect((await (await request.get(`${API}/runs/${id}`)).json()).status).toBe(pending ? 'pending' : 'running')
      await cancel.click()
      await dialog.getByRole('button', { name: 'Cancel run', exact: true }).click()
      await expect.poll(async () => (await (await request.get(`${API}/runs/${id}`)).json()).status).toBe('cancelled')
      await expect(dialog).toHaveCount(0)
    } finally {
      await rm(hold, { force: true })
      if (id) await request.delete(`${API}/runs/${id}`)
      await request.post(`${API}/queue/resume`, { data: {} })
    }
  })
}
