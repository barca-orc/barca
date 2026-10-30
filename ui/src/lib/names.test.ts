import { describe, expect, it } from 'vitest'
import { splitMiddle } from './names'

describe('splitMiddle', () => {
  it('leaves a short name whole', () => {
    expect(splitMiddle('invoices_0')).toEqual({ head: 'invoices_0', tail: '' })
  })

  it('keeps the end of a name that only just outgrows the tail', () => {
    expect(splitMiddle('check__credits_01')).toEqual({ head: 'check__', tail: '_credits_01'.slice(1) })
  })

  it('keeps the end of a long name, and loses nothing', () => {
    const { head, tail } = splitMiddle('check__risk_review_alerts_01')
    expect(tail).toBe('_alerts_01')
    expect(head + tail).toBe('check__risk_review_alerts_01')
  })
})
