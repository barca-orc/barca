import { existsSync } from 'node:fs'
import { defineConfig } from '@playwright/test'
import { fileURLToPath } from 'node:url'

const root = fileURLToPath(new URL('..', import.meta.url))
const fixture = fileURLToPath(new URL('./e2e-groups/fixture', import.meta.url))

// The grouped suite: `barca serve` on a project declared in groups (e2e-groups/fixture), with
// the Vite dev server in front of it, on ports of their own so it can run beside the flat
// suite's servers. The groups, states, runs and failures are the server's: nothing is stubbed.
const barca = process.env.BARCA_BIN ?? (existsSync(`${root}/target-local/debug/barca`) ? `${root}/target-local/debug/barca` : `${root}/target/debug/barca`)
const API = 'http://127.0.0.1:8277'
// The seed helpers (e2e/helpers/seed.ts) talk to this server; workers inherit the environment.
process.env.BARCA_E2E_API = API
export default defineConfig({
  testDir: './e2e-groups',
  // Its own artifacts: the flat suite's config clears test-results/ when it starts.
  outputDir: './test-results-groups',
  fullyParallel: false,
  workers: 1,
  reporter: 'list',
  use: { baseURL: 'http://127.0.0.1:5277', trace: 'retain-on-failure' },
  webServer: [
    {
      command: `"${barca}" serve --port 8277 --no-schedule`,
      cwd: fixture,
      env: { PYTHONPATH: `${root}/python`, PATH: process.env.PATH ?? '' },
      url: `${API}/state`,
      reuseExistingServer: !process.env.CI,
    },
    {
      command: 'pnpm exec vite --host 127.0.0.1 --port 5277 --strictPort',
      env: { BARCA_DEV_SERVER: API },
      url: 'http://127.0.0.1:5277/ui/',
      reuseExistingServer: !process.env.CI,
    },
  ],
})
