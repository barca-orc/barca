import { renderToStaticMarkup } from 'react-dom/server'
import { describe, expect, it } from 'vitest'
import { StepTimes, TraceLink } from './RunsView'

describe('TraceLink', () => {
  const step = { node_id: 'p.py:load', trace_id: '0123456789abcdef0123456789abcdef' }

  it('opens the step\'s trace in Datadog in a new tab', () => {
    const html = renderToStaticMarkup(<TraceLink step={step} datadogUrl="https://app.datadoghq.com" />)
    expect(html).toContain('href="https://app.datadoghq.com/apm/trace/0123456789abcdef0123456789abcdef"')
    expect(html).toContain('target="_blank"')
    expect(html).toContain('>View trace</a>')
  })

  it('is hidden when telemetry is off or the step sent no trace', () => {
    expect(renderToStaticMarkup(<TraceLink step={step} datadogUrl={null} />)).toBe('')
    expect(renderToStaticMarkup(<TraceLink step={{ node_id: 'p.py:load' }} datadogUrl="https://app.datadoghq.com" />)).toBe('')
  })
})

describe('StepTimes', () => {
  it('shows the time spent writing sinks beside the step\'s own time', () => {
    const html = renderToStaticMarkup(<StepTimes step={{ elapsed_seconds: 2, sink_seconds: 9.5 }} />)
    expect(html).toContain('>2.0s<')
    expect(html).toContain('+ 9.5s sinks')
  })

  it('shows no sink time for a step without sinks', () => {
    expect(renderToStaticMarkup(<StepTimes step={{ elapsed_seconds: 2, sink_seconds: null }} />)).not.toContain('sinks')
  })
})
