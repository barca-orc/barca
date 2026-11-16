import { describe, expect, it } from 'vitest'
import { documentTitle } from './documentTitle'

const title = (path: string, search = '', groups: Record<string, string> = {}) =>
  documentTitle(path, new URLSearchParams(search), (id) => groups[id] ?? null)

describe('documentTitle', () => {
  it('names the section, after what is open in it', () => {
    expect(title('/runs')).toBe('Runs · barca')
    expect(title('/runs', 'run=a7e9b0')).toBe('a7e9b0 · Runs · barca')
    expect(title('/assets')).toBe('Assets · barca')
    expect(title('/assets', 'pipeline=etl/billing.py')).toBe('billing.py · Assets · barca')
    expect(title('/tasks', 'pipeline=billing.py&node=billing.py:check__orders')).toBe('check__orders · Tasks · barca')
    expect(title('/schedules')).toBe('Schedules · barca')
  })
  it('names a group by its name, and by its id until the groups load', () => {
    expect(title('/assets', 'pipeline=ads.py&group=group:ads.py:g_raw', { 'group:ads.py:g_raw': 'Raw' })).toBe('Raw · Assets · barca')
    expect(title('/assets', 'group=group:ads.py:g_raw')).toBe('g_raw · Assets · barca')
  })
  it('names a key with its node in the key explorer', () => {
    expect(title('/keys', 'node=p.py:orders&key=day%3D2026-11-16')).toBe('day=2026-11-16 · orders · Keys · barca')
    expect(title('/keys', 'node=p.py:orders')).toBe('orders · Keys · barca')
  })
  it('is the app alone where no section is', () => {
    expect(title('/nowhere')).toBe('barca')
  })
})
