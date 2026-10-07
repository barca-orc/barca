import { defineConfig } from '@playwright/test'
import { fileURLToPath } from 'node:url'

const root = fileURLToPath(new URL('..', import.meta.url))
const fixture = fileURLToPath(new URL('./e2e/fixture', import.meta.url))

// Two servers, as in development: `barca serve` on the fixture project (API on
// :8274, which vite.config.ts proxies to) and the Vite dev server at /ui/.
// Build the binary first: `cargo build -p barca`.
export default defineConfig({
  testDir: './e2e',
  fullyParallel: false,
  workers: 1,
  reporter: 'list',
  use: { baseURL: 'http://localhost:5173', trace: 'retain-on-failure' },
  webServer: [
    {
      command: `${root}/target/debug/barca serve --no-schedule`,
      cwd: fixture,
      env: { PYTHONPATH: `${root}/python`, PATH: process.env.PATH ?? '' },
      url: 'http://127.0.0.1:8274/state',
      reuseExistingServer: !process.env.CI,
    },
    {
      command: 'pnpm exec vite --port 5173 --strictPort',
      url: 'http://localhost:5173/ui/',
      reuseExistingServer: !process.env.CI,
    },
  ],
})
