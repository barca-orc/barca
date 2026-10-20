import { expect, type APIRequestContext, type Request } from '@playwright/test'

const API = process.env.BARCA_E2E_API ?? 'http://127.0.0.1:8274'

/** Start a run through `POST /runs`, as the UI does (a task `run` refreshes its upstream). */
export async function startRun(request: APIRequestContext, target: string, verb: 'run' | 'get' = 'run'): Promise<string> {
  const data = verb === 'run'
    ? { command: 'run', targets: [target], refresh_all: true }
    : { command: 'get', targets: [target] }
  const res = await request.post(`${API}/runs`, { data })
  expect(res.ok()).toBeTruthy()
  const { run_id } = await res.json()
  return run_id
}

/** Wait until `GET /runs/{run_id}` shows a terminal status. */
export async function waitForRun(request: APIRequestContext, runId: string) {
  await expect
    .poll(async () => (await (await request.get(`${API}/runs/${encodeURIComponent(runId)}`)).json()).status, {
      timeout: 30_000,
    })
    .not.toMatch(/running|pending/)
}

/** Whether a browser request is the UI starting `command` on `target` (`POST /runs`). */
export function isStartRun(r: Request, command: 'run' | 'get', target?: string): boolean {
  if (r.method() !== 'POST' || !new URL(r.url()).pathname.endsWith('/runs')) return false
  const body = r.postDataJSON() as { command?: string; targets?: string[] } | null
  return body?.command === command && (target === undefined || (body.targets ?? []).includes(target))
}

/**
 * Run a task through the API `times` times and wait for each run to finish, so the node panel
 * has a history to show (the fixture's task sleeps a different time on each call).
 */
export async function seedRuns(request: APIRequestContext, target: string, times: number, verb: 'run' | 'get' = 'run') {
  for (let i = 0; i < times; i++) {
    await waitForRun(request, await startRun(request, target, verb))
  }
}
