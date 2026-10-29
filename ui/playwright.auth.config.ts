import { existsSync } from 'node:fs'
import { defineConfig } from '@playwright/test'
import { fileURLToPath } from 'node:url'

const root = fileURLToPath(new URL('..', import.meta.url))
const fixture = fileURLToPath(new URL('./e2e-auth/fixture', import.meta.url))
const barca = process.env.BARCA_BIN ?? (existsSync(`${root}/target-local/debug/barca`) ? `${root}/target-local/debug/barca` : `${root}/target/debug/barca`)
export default defineConfig({
  testDir: './e2e-auth',
  workers: 1,
  reporter: 'list',
  // Credential-bearing requests and the one-time secret must not enter trace artifacts.
  use: { baseURL: 'http://127.0.0.1:5276', trace: 'off' },
  webServer: [
    {
      command: `"${barca}" serve pipeline.py --port 8376 --no-schedule`,
      cwd: fixture,
      env: { PYTHONPATH: `${root}/python`, PATH: process.env.PATH ?? '' },
      url: 'http://127.0.0.1:8376/auth/status',
      reuseExistingServer: false,
    },
    {
      command: 'pnpm exec vite --host 127.0.0.1 --port 5276 --strictPort',
      env: { BARCA_DEV_SERVER: 'http://127.0.0.1:8376' },
      url: 'http://127.0.0.1:5276/ui/',
      reuseExistingServer: false,
    },
  ],
})
