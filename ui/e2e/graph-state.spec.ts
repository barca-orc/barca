import { test, expect } from '@playwright/test'
const assets=['source','normalize','validate','score','report'].map((name,i,names)=>({id:`demo.py:${name}`,kind:'asset',freshness: 'always',env:[],inputs:i?[`demo.py:${names[i-1]}`]:[]}))
test('linear intermediates remain visible with persistent status and deep links',async({page})=>{
  await page.route('**/assets',route=>route.fulfill({json:assets}))
  await page.route(/\/state(?:\?.*)?$/,route=>route.fulfill({json:assets.map((a,i)=>({...a,name:a.id.split(':')[1],partitioned:false,cache:{state:i===4?'never_run':'cached',reason:'materialized',detail:'demo'},last_materialization:i===2?{status:'failed',error:'validation failed',created_at:'today',elapsed_seconds:null,run_hash:null,artifact:null,format:null,size_bytes:null}:null,shape:null,env:[],durations:null,next_run:null}))}))
  await page.goto('/ui/#/graph')
  await expect(page.locator('.react-flow__node')).toHaveCount(5)
  // One state per node, in plain words: a failed last attempt shows as failing instead of the cache word.
  const report = page.locator('.react-flow__node').filter({hasText:'report'}).locator('.barca-dag')
  await expect(report).toHaveAttribute('data-tone','neutral')
  await expect(report).toContainText('never run')
  const validate = page.locator('.react-flow__node').filter({hasText:'validate'}).locator('.barca-dag')
  await expect(validate).toHaveAttribute('data-failed','true')
  await expect(validate).toContainText('failing')
  await expect(validate).not.toContainText('fresh')
  await page.getByRole('button',{name:'Top-down layout'}).click()
  await page.locator('.react-flow__node').filter({hasText:'validate'}).click()
  await expect(page.getByText('validation failed',{exact:true})).toBeVisible()
  await page.goto('/ui/#/graph?focus=demo.py:validate')
  await page.reload()
  await expect(page.locator('.react-flow__node')).toHaveCount(5)
})

test('failed state requests leave nodes neutral and fully visible',async({page})=>{
  await page.route('**/assets',route=>route.fulfill({json:assets}))
  await page.route(/\/state(?:\?.*)?$/,route=>route.fulfill({status:503,json:{error:'unavailable'}}))
  await page.goto('/ui/#/graph')
  await expect(page.getByText('Asset state unavailable; colors are neutral.')).toBeVisible()
  await expect(page.locator('.react-flow__node')).toHaveCount(5)
  await expect(page.locator('.react-flow__node .barca-dag[data-tone="neutral"]')).toHaveCount(5)
  await expect(page.locator('.react-flow__node .barca-dag[data-failed]')).toHaveCount(0)
})

test('a navigation keeps each card\'s last known state while the new scope\'s state loads', async ({ page }) => {
  const rows = assets.map((a, i) => ({ ...a, name: a.id.split(':')[1], partitioned: false, cache: { state: 'cached', reason: 'materialized', detail: 'demo' }, last_materialization: i === 2 ? { status: 'failed', error: 'validation failed', created_at: 'today', elapsed_seconds: null, run_hash: null, artifact: null, format: null, size_bytes: null } : null, shape: null, env: [], durations: null, next_run: null }))
  let release!: () => void
  const held = new Promise<void>((resolve) => { release = resolve })
  await page.route('**/assets', (route) => route.fulfill({ json: assets }))
  await page.route('**/groups', (route) => route.fulfill({ json: [] }))
  await page.route(/\/state(?:\?.*)?$/, async (route) => {
    // The pipeline's own state is held back; the whole project's answers at once.
    if (route.request().url().includes('files=')) await held
    await route.fulfill({ json: rows })
  })
  await page.goto('/ui/#/assets?view=graph')
  const validate = page.locator('.react-flow__node').filter({ hasText: 'validate' }).locator('.barca-dag')
  await expect(validate).toHaveAttribute('data-failed', 'true')
  await page.evaluate(() => { location.hash = '/assets?view=graph&pipeline=demo.py' })
  await expect(page).toHaveURL(/pipeline=demo\.py/)
  await page.waitForTimeout(300)
  await expect(page.locator('.react-flow__node')).toHaveCount(5)
  // Not one card reads Checking…: each keeps what it last said.
  await expect(page.locator('.react-flow__node .barca-dag[data-glyph="pending"]')).toHaveCount(0)
  await expect(validate).toHaveAttribute('data-failed', 'true')
  release()
})

test('only a page that has never loaded state shows Checking…', async ({ page }) => {
  let release!: () => void
  const held = new Promise<void>((resolve) => { release = resolve })
  await page.route('**/assets', (route) => route.fulfill({ json: assets }))
  await page.route('**/groups', (route) => route.fulfill({ json: [] }))
  await page.route(/\/state(?:\?.*)?$/, async (route) => {
    await held
    await route.fulfill({ json: [] })
  })
  await page.goto('/ui/#/assets?view=graph')
  await expect(page.locator('.react-flow__node .barca-dag[data-glyph="pending"]')).toHaveCount(5)
  release()
})
