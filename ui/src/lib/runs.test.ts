import { describe, expect, it } from 'vitest'
import { outcomeResult, parseOutcome, parseRunStatus, runIsLive, runLabel, runStatus, triggeredByLabel } from './runs'

describe('run status', () => {
  it('pending and running are live; every other status is terminal', () => {
    expect(runIsLive('pending')).toBe(true)
    expect(runIsLive('running')).toBe(true)
    for (const s of ['success', 'failed', 'cancelled', 'error', 'interrupted']) expect(runIsLive(s)).toBe(false)
  })
  it('maps each status to a visual status', () => {
    expect(runStatus('pending')).toBe('queued')
    expect(runStatus('running')).toBe('running')
    expect(runStatus('success')).toBe('success')
    expect(runStatus('failed')).toBe('failed')
    expect(runStatus('error')).toBe('failed')
    expect(runStatus('cancelled')).toBe('skipped')
    expect(runStatus('interrupted')).toBe('skipped')
    expect(runStatus('something new')).toBe('warning')
  })
  it('reads older spellings of success', () => {
    expect(parseRunStatus('complete')).toBe('success')
    expect(parseRunStatus('ran')).toBe('success')
    expect(parseRunStatus('nope')).toBeNull()
  })
  it('labels pending as queued and keeps unknown statuses as given', () => {
    expect(runLabel('pending')).toBe('queued')
    expect(runLabel('complete')).toBe('success')
    expect(runLabel('weird')).toBe('weird')
  })
})

describe('triggeredByLabel', () => {
  it('names what started a run, a dash when unknown', () => {
    expect(triggeredByLabel('schedule')).toBe('schedule')
    expect(triggeredByLabel('api')).toBe('api')
    expect(triggeredByLabel('client')).toBe('client')
    expect(triggeredByLabel('manual')).toBe('manual')
    expect(triggeredByLabel(null)).toBe('–')
  })
})

describe('parseOutcome', () => {
  it('is null without an outcome', () => {
    expect(parseOutcome(null)).toBeNull()
    expect(parseOutcome(undefined)).toBeNull()
  })
  it('reads a success with its value', () => {
    const outcome = parseOutcome({ status: 'success', run_id: '7', steps: [], final_output: { rows: 3 } })
    expect(outcome).toEqual({ status: 'success', run_id: '7', final_output: { rows: 3 } })
    expect(outcomeResult(outcome)).toEqual({ rows: 3 })
  })
  it('reads several targets', () => {
    const outcome = parseOutcome({ status: 'success', run_id: '7', targets: { a: 1, b: 2 } })
    expect(outcomeResult(outcome)).toEqual({ a: 1, b: 2 })
  })
  it('reads a non-success envelope', () => {
    const outcome = parseOutcome({
      status: 'error',
      envelope: { error: 'worker died', code: 3, kind: 'infra', remediation: 'check the worker log' },
    })
    expect(outcome).toEqual({
      status: 'error',
      envelope: { error: 'worker died', code: 3, kind: 'infra', remediation: 'check the worker log', node: null, traceback: null, artifact_dir: null },
    })
    expect(outcomeResult(outcome)).toBeUndefined()
  })
  it('is null for a shape it does not know', () => {
    expect(parseOutcome({ status: 'failed' })).toBeNull()
    expect(parseOutcome({ status: 'mystery' })).toBeNull()
  })
})
