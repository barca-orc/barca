import { describe, it, expect } from 'vitest'
import { renderToStaticMarkup } from 'react-dom/server'
import { AssetVersionsView, type VersionsConfirm } from './AssetVersions'
import type { AssetVersion, AssetVersions } from '@/lib/types'

const noop = () => {}
const NOW = Date.parse('2026-11-07T12:00:00Z')

const version = (run_hash: string, extra: Partial<AssetVersion> = {}): AssetVersion => ({
  run_hash,
  run_id: `run-${run_hash.slice(0, 4)}`,
  materialized_at: '2026-11-07T11:00:00Z',
  elapsed_seconds: 1.5,
  size_bytes: 2048,
  format: 'json',
  artifact: `.barca/artifacts/p.py--orders/${run_hash}.json`,
  available: true,
  current: false,
  pinned: false,
  ...extra,
})

const data = (over: Partial<AssetVersions> = {}): AssetVersions => ({
  node_id: 'p.py:orders',
  pinnable: true,
  pin: null,
  versions: [version('aaaaaaaaaaaaaaaa', { current: true }), version('bbbbbbbbbbbbbbbb'), version('cccccccccccccccc', { available: false })],
  ...over,
})

const render = (d: AssetVersions, opts: { readOnly?: boolean; confirm?: VersionsConfirm | null; error?: string | null } = {}) =>
  renderToStaticMarkup(
    <AssetVersionsView name="orders" data={d} nowMs={NOW} readOnly={opts.readOnly ?? false} pending={false}
      error={opts.error ?? null} confirm={opts.confirm ?? null} onAsk={noop} onConfirm={noop} onCancel={noop} />,
  )
const text = (html: string) => html.replace(/<[^>]+>/g, '')
const buttons = (html: string) => [...html.matchAll(/<button[^>]*>(.*?)<\/button>/g)].map((m) => ({ disabled: /disabled=""/.test(m[0]), label: text(m[1] ?? '') }))

describe('AssetVersionsView', () => {
  it('lists every version with its run, duration and size, and offers a rollback on each but the pinned one', () => {
    const html = render(data())
    expect(text(html)).toContain('aaaaaaaaaaaa')
    expect(text(html)).toContain('run run-aaaa')
    expect(text(html)).toContain('1.5s')
    expect(text(html)).toContain('2.0 KB')
    expect(text(html)).toContain('latest')
    expect(text(html)).toContain('evicted')
    const rollbacks = buttons(html).filter((b) => b.label === 'Roll back to this')
    expect(rollbacks).toHaveLength(3)
    // An evicted version cannot be pinned.
    expect(rollbacks.map((b) => b.disabled)).toEqual([false, false, true])
    expect(html).not.toContain('Pinned at')
  })

  it('shows who pinned it, when and why, with Unpin, while pinned', () => {
    const pinned = data({
      pin: { node_id: 'p.py:orders', run_hash: 'bbbbbbbbbbbbbbbb', pinned_at: '2026-11-07T11:30:00Z', pinned_by: 'token:ops', reason: 'bad feed' },
      versions: [version('aaaaaaaaaaaaaaaa', { current: true }), version('bbbbbbbbbbbbbbbb', { pinned: true })],
    })
    const html = render(pinned)
    expect(text(html)).toContain('Pinned at bbbbbbbbbbbb')
    expect(text(html)).toContain('by token:ops: bad feed')
    expect(buttons(html).map((b) => b.label)).toEqual(['Unpin', 'Roll back to this'])
  })

  it('disables every action on a read-only server', () => {
    const html = render(data({ pin: { node_id: 'p.py:orders', run_hash: 'aaaaaaaaaaaaaaaa', pinned_at: '2026-11-07T11:30:00Z', pinned_by: null, reason: null } }), { readOnly: true })
    expect(buttons(html).every((b) => b.disabled)).toBe(true)
  })

  it('asks before pinning and before unpinning', () => {
    const v = version('bbbbbbbbbbbbbbbb')
    const pin = render(data(), { confirm: { kind: 'pin', version: v } })
    expect(pin).toContain('role="dialog"')
    expect(text(pin)).toContain('Pin orders to bbbbbbbbbbbb?')
    expect(text(pin)).toContain('run run-bbbb')
    expect(buttons(pin).map((b) => b.label)).toContain('Pin this version')
    const unpin = render(data(), { confirm: { kind: 'unpin' } })
    expect(text(unpin)).toContain('Unpin orders?')
  })

  it('says why a partitioned asset has no rollback, and shows an error', () => {
    const html = render(data({ pinnable: false }), { error: 'HTTP 403' })
    expect(buttons(html).filter((b) => b.label === 'Roll back to this')).toHaveLength(0)
    expect(text(html)).toContain('Only an asset without partitions can be pinned.')
    expect(text(html)).toContain('Could not change the pin: HTTP 403')
  })
})
