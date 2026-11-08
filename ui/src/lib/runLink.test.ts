import { describe, expect, it } from 'vitest'
import { runLink } from './runRecovery'

describe('runLink', () => {
  it('links to the run through the hash route, the way the UI is served', () =>
    expect(runLink('943a86e83aac', 'http://localhost:8274/ui/#/runs?status=failed')).toBe('http://localhost:8274/ui/#/runs?run=943a86e83aac'))
  it('keeps a reverse-proxy prefix and drops any query', () =>
    expect(runLink('a b', 'https://host/barca/ui/?x=1#/assets')).toBe('https://host/barca/ui/#/runs?run=a%20b'))
})
