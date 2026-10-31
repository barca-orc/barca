import { describe, expect, it } from 'vitest'
import type { PolledQuery } from './polling'
import { RECOVER_MS, recovering } from './polling'

const at = (status: 'pending' | 'error' | 'success', data?: unknown) => ({ state: { status, data } }) as PolledQuery<unknown>

describe('recovering', () => {
  it('polls a failed query until it answers', () => {
    expect(recovering()(at('error'))).toBe(RECOVER_MS)
    expect(recovering(10_000)(at('error'))).toBe(RECOVER_MS)
  })

  it('keeps the hook’s own interval otherwise', () => {
    expect(recovering()(at('success', []))).toBe(false)
    expect(recovering(10_000)(at('success', []))).toBe(10_000)
    expect(recovering(10_000)(at('pending'))).toBe(10_000)
  })

  it('passes a dynamic interval the query', () => {
    const fast = recovering<number[]>((q) => (q.state.data?.length ? 2_000 : 10_000))
    expect(fast(at('success', [1]) as PolledQuery<number[]>)).toBe(2_000)
    expect(fast(at('success', []) as PolledQuery<number[]>)).toBe(10_000)
    expect(fast(at('error') as PolledQuery<number[]>)).toBe(RECOVER_MS)
  })
})
