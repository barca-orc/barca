import { useState } from 'react'
import { incidentSummary } from '@/lib/runRecovery'
import type { RunView } from '@/lib/types'
import { Button } from './Button'

/** The run's deep link: this page with `?run=<id>` and nothing else. */
function runLink(runId: string): string {
  const url = new URL(window.location.href)
  url.search = new URLSearchParams({ run: runId }).toString()
  url.hash = ''
  return url.toString()
}

/** "Copy incident summary": the run as markdown, for an incident or a chat (`incidentSummary`). */
export function IncidentSummaryButton({ run }: { run: RunView }) {
  const [state, setState] = useState<'idle' | 'copied' | 'failed'>('idle')
  const copy = () => {
    const text = incidentSummary(run, runLink(run.run_id))
    const done = (s: 'copied' | 'failed') => { setState(s); setTimeout(() => setState('idle'), 1500) }
    if (!navigator.clipboard) return done('failed')
    navigator.clipboard.writeText(text).then(() => done('copied'), () => done('failed'))
  }
  return <Button size="sm" variant="ghost" onClick={copy}>
    {state === 'copied' ? 'Copied' : state === 'failed' ? 'Copy failed' : 'Copy incident summary'}
  </Button>
}
