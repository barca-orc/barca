import { useState } from 'react'
import { Check, Copy } from 'lucide-react'
import { incidentSummary, runLink } from '@/lib/runRecovery'
import type { RunView } from '@/lib/types'
import { IconButton } from './IconButton'

/** "Copy incident summary": the run as markdown, for an incident or a chat (`incidentSummary`). An icon with its name as the tooltip. */
export function IncidentSummaryButton({ run, datadogUrl }: { run: RunView; datadogUrl?: string | null }) {
  const [state, setState] = useState<'idle' | 'copied' | 'failed'>('idle')
  const copy = () => {
    const text = incidentSummary(run, runLink(run.run_id), datadogUrl)
    const done = (s: 'copied' | 'failed') => { setState(s); setTimeout(() => setState('idle'), 1500) }
    if (!navigator.clipboard) return done('failed')
    navigator.clipboard.writeText(text).then(() => done('copied'), () => done('failed'))
  }
  return <IconButton size="sm" label={state === 'copied' ? 'Copied' : state === 'failed' ? 'Copy failed' : 'Copy incident summary'} onClick={copy}>
    {state === 'copied' ? <Check size={14} /> : <Copy size={14} />}
  </IconButton>
}
