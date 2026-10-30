import { describe, it, expect } from 'vitest'
import { remote } from './remote'

describe('remote', () => {
  it('folds a query into one of three states', () => {
    expect(remote({ data: undefined, error: null })).toEqual({ kind: 'loading' })
    expect(remote({ data: undefined, error: new Error('down') })).toEqual({ kind: 'failed', error: 'down' })
    expect(remote({ data: [1], error: null })).toEqual({ kind: 'ready', data: [1], refreshError: null })
  })

  it('keeps loaded data when a refetch fails', () => {
    expect(remote({ data: [1], error: new Error('down') })).toEqual({ kind: 'ready', data: [1], refreshError: 'down' })
  })

  it('treats placeholder data as nothing loaded', () => {
    expect(remote({ data: [1], error: null, isPlaceholderData: true })).toEqual({ kind: 'loading' })
  })
})
