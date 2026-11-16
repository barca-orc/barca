import { existsSync } from 'node:fs'
import { defineConfig } from '@playwright/test'
import { fileURLToPath } from 'node:url'

const root = fileURLToPath(new URL('..', import.meta.url))
const fixture = fileURLToPath(new URL('./e2e/fixture', import.meta.url))

// Two servers, as in development: `barca serve` on the fixture project (API on
// :8274, which vite.config.ts proxies to) and the Vite dev server at /ui/.
// Build the binary first (`cargo build -p barca`), or point BARCA_BIN at one.
// Ports default to the development ones; set BARCA_E2E_PORT / BARCA_E2E_UI_PORT to run beside them.
const apiPort = process.env.BARCA_E2E_PORT ?? '8274'
const uiPort = process.env.BARCA_E2E_UI_PORT ?? '5173'
const API = `http://127.0.0.1:${apiPort}`
// The specs and the seed helpers (e2e/helpers/seed.ts) talk to this server; workers inherit it.
process.env.BARCA_E2E_API = API
const barca = process.env.BARCA_BIN ?? (existsSync(`${root}/target-local/debug/barca`) ? `${root}/target-local/debug/barca` : `${root}/target/debug/barca`)
export default defineConfig({
  testDir: './e2e',
  fullyParallel: false,
  workers: 1,
  reporter: 'list',
  use: { baseURL: `http://localhost:${uiPort}`, trace: 'retain-on-failure' },
  webServer: [
    {
      command: `"${barca}" serve --no-schedule --port ${apiPort}`,
      cwd: fixture,
      env: { PYTHONPATH: `${root}/python`, PATH: process.env.PATH ?? '' },
      url: `${API}/state`,
      reuseExistingServer: !process.env.CI,
    },
    {
      command: `pnpm exec vite --port ${uiPort} --strictPort`,
      env: { BARCA_DEV_SERVER: API },
      url: `http://localhost:${uiPort}/ui/`,
      reuseExistingServer: !process.env.CI,
    },
  ],
})
