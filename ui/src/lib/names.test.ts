import { describe, expect, it } from 'vitest'
import { splitMiddle } from './names'

describe('splitMiddle', () => {
  it('leaves a short name whole', () => {
    expect(splitMiddle('invoices_00')).toEqual({ head: 'invoices_00', tail: '' })
  })

  it('keeps the end of a long name, and loses nothing', () => {
    const { head, tail } = splitMiddle('check__risk_review_alerts_01')
    expect(tail).toBe('_alerts_01')
    expect(head + tail).toBe('check__risk_review_alerts_01')
  })
})
