// Live HTTP acceptance for #1044. Run against audit1044-fixture.py after seeding.
// node ui/scripts/audit1044-measure.mjs http://localhost:15444 /path/to/evidence
import { chromium } from '@playwright/test'
import { mkdir, writeFile } from 'node:fs/promises'
const origin = process.argv[2] ?? 'http://localhost:15444'
const destination = process.argv[3] ?? '/tmp/barca1044-evidence'
await mkdir(destination, { recursive: true })
const browser = await chromium.launch({ headless: true })
const page = await browser.newPage({ viewport: { width: 1440, height: 900 }, colorScheme: 'light' })
const cdp = await page.context().newCDPSession(page)
await cdp.send('Network.enable')
let phase = 'firstLoad'
const totals = { firstLoad: 0, idle: 0, flatIdle: 0 }
const apiBytes = { firstLoad: 0, idle: 0, flatIdle: 0 }
const requests = []
const paths = new Map()
const failures = []
cdp.on('Network.requestWillBeSent', x => {
  const path = new URL(x.request.url).pathname
  paths.set(x.requestId, { path, phase })
  if (!path.startsWith('/ui/') && (phase === 'idle' || phase === 'flatIdle')) requests.push(new URL(x.request.url).pathname + new URL(x.request.url).search)
})
cdp.on('Network.loadingFinished', x => {
  const entry = paths.get(x.requestId)
  if (entry && entry.phase in totals) {
    totals[entry.phase] += x.encodedDataLength
    if (!entry.path.startsWith('/ui/')) apiBytes[entry.phase] += x.encodedDataLength
  }
})
page.on('pageerror', error => failures.push(error.message))
try {
  await page.goto(`${origin}/ui/#/assets`)
  await page.getByText('Audit 0', { exact: true }).first().waitFor()
  await page.waitForTimeout(5000)
  phase = 'idle'
  await page.waitForTimeout(30000)
  phase = 'layout'
  if (totals.idle >= 500000) failures.push(`idle transfer ${totals.idle} >= 500000`)
  if (requests.includes('/state')) failures.push('whole state polled while idle')
  if (await page.locator('html').getAttribute('data-theme') !== 'light') failures.push('OS light theme not respected')
  await page.screenshot({ path: `${destination}/assets-1440.png` })
  const clipped = await page.locator('.barca-health-label, .gcol-contents, .barca-estimate').evaluateAll(elements => elements.filter(e => e.getClientRects().length && getComputedStyle(e).visibility !== 'hidden' && e.scrollWidth > e.clientWidth + 1).map(e => e.textContent))
  if (clipped.length) failures.push(`clipped asset details: ${clipped.join('; ')}`)
  await page.goto(`${origin}/ui/#/assets?flat=1`)
  await page.waitForTimeout(5000)
  phase = 'flatIdle'
  await page.waitForTimeout(30000)
  phase = 'layout'
  if (totals.flatIdle >= 500000) failures.push(`flat idle transfer ${totals.flatIdle} >= 500000`)
  if (requests.includes('/state')) failures.push('whole state polled in flat view')
  await page.screenshot({ path: `${destination}/assets-flat-1440.png` })
  const clippedDurations = await page.locator('td.gcol-duration').evaluateAll(elements => elements.filter(e => e.scrollWidth > e.clientWidth + 1).map(e => e.textContent))
  if (clippedDurations.length) failures.push(`clipped duration: ${clippedDurations.join('; ')}`)
  const blankEstimates = await page.locator('.barca-group-table .barca-estimate').evaluateAll(elements => elements.filter(e => !e.textContent.trim()).length)
  if (blankEstimates) failures.push('blank Fresh by cells')
  await page.goto(`${origin}/ui/#/assets?view=graph&flat=1&focus=pipeline.py%3Aasset_0000`)
  const root = page.locator('.react-flow__node[data-id="pipeline.py:asset_0000"]')
  await root.waitFor()
  await page.waitForTimeout(1000)
  const rootBox = await root.boundingBox()
  const canvasBox = await page.locator('.react-flow').boundingBox()
  if (!rootBox || !canvasBox || rootBox.x < canvasBox.x || rootBox.x + rootBox.width > canvasBox.x + canvasBox.width) failures.push('failing graph root clipped')
  await page.screenshot({ path: `${destination}/graph-root-1440.png` })
  await page.goto(`${origin}/ui/#/schedules`)
  await page.getByText('*/15 * * * *', { exact: true }).first().waitFor()
  await page.screenshot({ path: `${destination}/schedules-1440.png` })
  const cronClipped = await page.locator('.barca-schedule-cron').evaluateAll(elements => elements.filter(e => e.scrollWidth > e.clientWidth + 1).map(e => e.textContent))
  if (cronClipped.length) failures.push(`clipped cron: ${cronClipped.join('; ')}`)
  await page.goto(`${origin}/ui/#/keys?node=pipeline.py%3Aasset_0006&empty=1&show=all`)
  await page.getByRole('columnheader', { name: 'Rows', exact: true }).waitFor()
  await page.getByLabel('Empty results (0 rows)').check()
  const response = await page.request.get(`${origin}/assets/pipeline.py%3Aasset_0006/partitions/keys?empty=1&limit=100`)
  const keys = await response.json()
  if (keys.total !== 51 || keys.keys.some(k => k.rows !== 0)) failures.push(`empty keys mismatch: ${keys.total}`)
  await page.screenshot({ path: `${destination}/keys-1440.png` })
  await page.emulateMedia({ colorScheme: 'dark' })
  await page.waitForTimeout(100)
  if (await page.locator('html').getAttribute('data-theme') === 'light') failures.push('OS theme change ignored')
  await page.getByRole('button', { name: 'Switch to light', exact: true }).click()
  await page.reload()
  if (await page.locator('html').getAttribute('data-theme') !== 'light') failures.push('explicit theme preference lost')
} catch (error) { failures.push(String(error)) }
await browser.close()
const result = { fixture: 'equivalent, not the original lost audit fixture', viewport: '1440x900', windowSeconds: 30, limitBytes: 500000, bytes: totals, apiBytes, idleRequests: requests, failures, pass: failures.length === 0 }
await writeFile(`${destination}/result.json`, JSON.stringify(result, null, 2) + '\n')
console.log(JSON.stringify(result))
if (failures.length) process.exitCode = 1
