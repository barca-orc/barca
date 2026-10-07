import { expect, type APIRequestContext } from '@playwright/test'

const API = process.env.BARCA_E2E_API ?? 'http://127.0.0.1:8274'

/**
 * Run a task through the API `times` times and wait for each run to finish, so the node panel
 * has a history to show (the fixture's task sleeps a different time on each call).
 */
export async function seedRuns(request: APIRequestContext, task: string, times: number) {
  for (let i = 0; i < times; i++) {
    const res = await request.post(`${API}/run/${task}`)
    const { run_id } = await res.json()
    await expect
      .poll(async () => (await (await request.get(`${API}/status/${run_id}`)).json()).status, {
        timeout: 30_000,
      })
      .not.toMatch(/running|queued|pending/)
  }
}
