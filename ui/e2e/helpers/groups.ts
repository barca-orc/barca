import { readFileSync } from 'node:fs'
import type { Page } from '@playwright/test'

/** The file the stub serves the demo's nodes under: not `modeling.py`, whose header carries the
 * demo-only "Preview failed check" control that no real project has. */
export const STUB_PIPELINE = 'forecast.py'

const metadata = JSON.parse(
  readFileSync(new URL('../group-metadata.json', import.meta.url), 'utf8').replaceAll('modeling.py', STUB_PIPELINE),
)
const source = readFileSync(new URL('../../demo/modeling.py', import.meta.url), 'utf8')
const names = [...source.matchAll(/^def ((?:validate__)?(?:data|cv|train|test|release)__\w+)\(/gm)].map((m) => m[1]!)

/** What every fold reads from the split group: a fold scope then has two external inputs. */
const FOLD_INPUTS = [`${STUB_PIPELINE}:data__train_rows`, `${STUB_PIPELINE}:data__fold_assignments`]

/**
 * Serve the demo's modeling pipeline (ui/demo/modeling.py, grouped by group-metadata.json) from
 * the browser, as groups.spec.ts does: the e2e server's own project has no groups.
 */
export async function stubModelingGroups(page: Page) {
  const assets = names.map((name) => ({
    id: `${STUB_PIPELINE}:${name}`,
    kind: name.startsWith('validate__') ? 'task' : 'asset',
    inputs: name.startsWith('validate__')
      ? [`${STUB_PIPELINE}:${name.replace(/^validate__/, '')}`]
      : name.startsWith('cv__fold_') ? FOLD_INPUTS : [],
    freshness: 'always',
    env: [],
  }))
  await page.route('**/groups', (route) => route.fulfill({ json: metadata }))
  await page.route('**/assets', (route) => route.fulfill({ json: assets }))
  await page.route(/\/state(?:\?.*)?$/, (route) =>
    route.fulfill({
      json: assets.map((asset) => ({
        ...asset,
        name: asset.id.split(':')[1],
        partitioned: false,
        shape: null,
        durations: null,
        next_run: null,
        cache: {
          state: asset.kind === 'task' ? 'always_runs' : 'cached',
          reason: asset.kind === 'task' ? 'task' : 'materialized',
          detail: 'Demo state',
        },
        last_materialization: {
          status: 'success',
          created_at: '2026-10-09 00:00:00',
          elapsed_seconds: null,
          run_hash: null,
          artifact: null,
          format: null,
          size_bytes: null,
        },
      })),
    }),
  )
}
