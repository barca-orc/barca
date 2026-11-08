import { describe, expect, it } from 'vitest'
import { pinNote, pinNotes, pinnedBanner } from './pins'
import type { AssetPin } from './types'

const pin = (over: Partial<AssetPin> = {}): AssetPin => ({
  node_id: 'p.py:orders',
  run_hash: 'aaaaaaaaaaaaaaaaaaaa',
  pinned_at: '2026-11-07T11:00:00Z',
  pinned_by: null,
  reason: null,
  run_id: null,
  upload: null,
  ...over,
})

describe('pins', () => {
  it('says what a pin holds and who pinned it', () => {
    const upload = { uploaded_at: '2026-11-07T11:00:00Z', uploaded_by: 'me@host', file_name: 'fix.json', reason: null }
    expect(pinNote(pin({ upload, pinned_by: 'me@host' }))).toBe('override from fix.json by me@host')
    expect(pinNote(pin({ run_id: '9738f63facde1234' }))).toBe('rolled back to 9738f63facde')
    expect(pinNote(pin())).toBe('rolled back to aaaaaaaaaaaa')
    expect(pinNotes({ pins: [pin()] }).get('p.py:orders')).toBe('rolled back to aaaaaaaaaaaa')
  })

  it('has a banner only while something is pinned', () => {
    expect(pinnedBanner(0)).toBeNull()
    expect(pinnedBanner(1)?.headline).toBe('1 asset pinned')
    expect(pinnedBanner(3)).toEqual({ headline: '3 assets pinned', detail: '· runs read pinned versions' })
  })
})
