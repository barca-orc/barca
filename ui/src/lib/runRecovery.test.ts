import { describe, expect, it } from 'vitest'
import type { LogLine, RunStep, RunView } from './types'
import {
  filterLogs, highlightParts, incidentSummary, traceUrl, isErrorLine, logsText, NO_LOG_FILTER, partitionArg, raisedAt,
  rerunAllRequest, rerunFailedRequest, retryStepRequest, splitStepId,
} from './runRecovery'

const lines: LogLine[] = [
  { nodeId: 'p.py:load', text: 'loading rows', stream: 'stdout' },
  { nodeId: 'p.py:regional[region=amer]', text: 'computing amer', stream: 'stdout' },
  { nodeId: 'p.py:regional[region=emea]', text: 'computing emea', stream: 'stdout' },
  { nodeId: 'p.py:load', text: 'Traceback (most recent call last):', stream: 'stderr' },
  { nodeId: 'p.py:other', text: 'ValueError: bad row', stream: 'stdout' },
]

describe('log filters', () => {
  it('keeps a step\'s own lines, and every key\'s for a node id without one', () => {
    expect(filterLogs(lines, { ...NO_LOG_FILTER, step: 'p.py:load' }).map((l) => l.number)).toEqual([1, 4])
    expect(filterLogs(lines, { ...NO_LOG_FILTER, step: 'p.py:regional' }).map((l) => l.number)).toEqual([2, 3])
    expect(filterLogs(lines, { ...NO_LOG_FILTER, step: 'p.py:regional[region=amer]' }).map((l) => l.number)).toEqual([2])
  })
  it('searches case-insensitively and keeps the original line numbers', () => {
    expect(filterLogs(lines, { ...NO_LOG_FILTER, query: 'COMPUTING E' })).toEqual([{ line: lines[2]!, number: 3 }])
  })
  it('errors only: stderr lines and traceback text on stdout', () => {
    expect(isErrorLine({ text: 'fine', stream: 'stderr' })).toBe(true)
    expect(isErrorLine({ text: 'ValueError: x', stream: 'stdout' })).toBe(true)
    expect(isErrorLine({ text: 'no Errors here', stream: 'stdout' })).toBe(false)
    expect(filterLogs(lines, { ...NO_LOG_FILTER, errorsOnly: true }).map((l) => l.number)).toEqual([4, 5])
  })
  it('highlights every match', () => {
    expect(highlightParts('Amer and amer', 'amer')).toEqual([
      { text: 'Amer', match: true }, { text: ' and ', match: false }, { text: 'amer', match: true },
    ])
    expect(highlightParts('x', ' ')).toEqual([{ text: 'x', match: false }])
  })
  it('copies lines with their step named', () => {
    expect(logsText([lines[3]!])).toBe('[p.py:load] (stderr) Traceback (most recent call last):\n')
  })
})

const run = { command: 'get', target: 'a,b', files: [] } as Pick<RunView, 'command' | 'target' | 'files'>

describe('re-run requests', () => {
  it('re-runs the same target cache-aware, or refreshed', () => {
    expect(rerunFailedRequest(run)).toEqual({ command: 'get', targets: ['a', 'b'], files: [] })
    expect(rerunAllRequest({ ...run, target: null })).toEqual({ command: 'get', targets: [], files: [], refresh_all: true })
  })
  it('retries one step, one key of a partitioned node by its key', () => {
    expect(splitStepId('p.py:r[region=amer]')).toEqual({ node: 'p.py:r', key: 'region=amer' })
    expect(retryStepRequest(run, 'p.py:r[region=amer]')).toEqual({ command: 'get', targets: ['p.py:r'], files: [], partition: 'region=amer' })
    expect(partitionArg('region=amer,day=1')).toBe('{"region":"amer","day":"1"}')
    expect(retryStepRequest({ ...run, command: 'run', target: 'publish' }, 'p.py:publish').command).toBe('run')
  })
})

describe('incident summary', () => {
  const tb = '  File "/x/p.py", line 3, in helper\n  File "/x/p.py", line 9, in load\n    raise ValueError("bad")'
  const step = { node_id: 'p.py:load', status: 'failed', error: 'ValueError: bad\nmore', error_type: 'ValueError', error_message: 'bad', error_traceback: tb } as RunStep
  const view = {
    run_id: 'r1', status: 'failed', command: 'get', target: 'load', triggered_by: 'schedule',
    started_at: '2026-11-07T10:00:00Z', elapsed_seconds: 75, error: 'ValueError: bad', failure: null,
    steps: [step, { node_id: 'p.py:ok', status: 'success' } as RunStep],
  } as unknown as RunView
  it('names the run, its link, target, trigger, timing and each failed step with where it raised', () => {
    expect(raisedAt(tb)).toBe('/x/p.py:9 in load')
    const md = incidentSummary(view, 'http://h/runs?run=r1')
    expect(md).toContain('[r1](http://h/runs?run=r1)')
    expect(md).toContain('`get load`')
    expect(md).toContain('**Triggered by:** schedule')
    expect(md).toContain('**Started:** 2026-11-07T10:00:00Z')
    expect(md).toContain('**Duration:** 1m 15s')
    expect(md).toContain('- `p.py:load`: ValueError: bad (raised at `/x/p.py:9 in load`)')
    expect(md).not.toContain('p.py:ok')
  })
  it('links each failed step to its Datadog trace when the server names a site', () => {
    const traced = { ...view, steps: [{ ...step, trace_id: 'ab'.repeat(16) }] } as unknown as RunView
    const md = incidentSummary(traced, 'http://h/runs?run=r1', 'https://app.datadoghq.eu')
    expect(md).toContain(`(raised at \`/x/p.py:9 in load\`) · [trace](https://app.datadoghq.eu/apm/trace/${'ab'.repeat(16)})`)
    expect(incidentSummary(traced, 'http://h/runs?run=r1', null)).not.toContain('[trace]')
    expect(incidentSummary(view, 'http://h/runs?run=r1', 'https://app.datadoghq.eu')).not.toContain('[trace]')
  })
})

describe('trace links', () => {
  it('needs both a Datadog app and a trace id', () => {
    expect(traceUrl('https://us3.datadoghq.com/', { trace_id: 'f0' })).toBe('https://us3.datadoghq.com/apm/trace/f0')
    expect(traceUrl(null, { trace_id: 'f0' })).toBeNull()
    expect(traceUrl('https://app.datadoghq.com', {})).toBeNull()
  })
})
