import { renderToStaticMarkup } from 'react-dom/server'
import { describe, expect, it } from 'vitest'
import { TraceLink } from './RunsView'

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
