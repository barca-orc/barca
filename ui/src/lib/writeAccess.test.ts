import { describe, expect, it } from 'vitest'
import { writeAccess, writeControl } from './writeAccess'

describe('writeControl', () => {
  it('hides write actions on a read-only server, disables them until it is known, enables them otherwise', () => {
    expect(writeControl(writeAccess({ read_only: true }))).toBe('hidden')
    expect(writeControl(writeAccess(undefined))).toBe('disabled')
    expect(writeControl(writeAccess({ read_only: false }))).toBe('enabled')
  })
})
