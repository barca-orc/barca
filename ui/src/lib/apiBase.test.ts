import { describe, expect, it } from 'vitest'
import { apiBase, apiUrl } from './apiBase'

describe('apiBase', () => {
  it('is the server root when the UI is served at /ui/', () => {
    expect(apiBase('/ui/')).toBe('')
    expect(apiBase('/ui')).toBe('')
  })

  it('keeps a reverse-proxy prefix', () => {
    expect(apiBase('/barca/ui/')).toBe('/barca')
    expect(apiBase('/tools/barca/ui/')).toBe('/tools/barca')
    expect(apiBase('/barca/ui')).toBe('/barca')
  })

  it('only strips a whole trailing `ui` segment', () => {
    // A prefix that merely ends in "ui" is not the UI segment.
    expect(apiBase('/gui/ui/')).toBe('/gui')
    expect(apiBase('/build/')).toBe('/build')
  })

  it('treats index.html as the UI page itself', () => {
    expect(apiBase('/barca/ui/index.html')).toBe('/barca')
  })

  it('keeps API requests outside the UI on dev-server fallback paths', () => {
    expect(apiUrl(apiBase('/ui/assets'), '/state')).toBe('/state')
    expect(apiUrl(apiBase('/barca/ui/assets/'), '/state')).toBe('/barca/state')
    expect(apiBase('/tools/barca/ui/graph')).toBe('/tools/barca')
  })
})

describe('apiUrl', () => {
  it('joins the base and an API path', () => {
    expect(apiUrl('/barca', '/state')).toBe('/barca/state')
    expect(apiUrl('', '/events/abc')).toBe('/events/abc')
  })
})
