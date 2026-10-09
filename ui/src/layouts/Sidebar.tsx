import { Link, useLocation, useNavigate, useSearchParams } from 'react-router'
import { Layers } from 'lucide-react'
import { StatusDot } from '@/components'
import { useAssetStates } from '@/hooks/useAssetStates'
import { severityStatus } from '@/lib/assetTable'
import { pipelineSummaries } from '@/lib/pipeline'
import markUrl from '@/assets/brand/mark.svg'

/** Pages that can show one pipeline; elsewhere a pipeline click opens Assets. */
const FILTERABLE = ['/assets', '/graph', '/tasks', '/sensors', '/schedules']

export function Sidebar() {
  const { data: nodes } = useAssetStates()
  const pipelines = pipelineSummaries(nodes ?? [])
  const location = useLocation()
  const navigate = useNavigate()
  const [params] = useSearchParams()
  // The selected pipeline lives in the URL (?pipeline=<file>) — the one source
  // the Assets table and the graph both read.
  const selected = params.get('pipeline')

  const select = (file: string | null) => {
    const next = new URLSearchParams(params)
    if (file === null) next.delete('pipeline')
    else next.set('pipeline', file)
    next.delete('node')
    next.delete('focus')
    next.delete('group')
    next.delete('q')
    next.delete('attention')
    const path = FILTERABLE.includes(location.pathname) ? location.pathname : '/assets'
    const search = next.toString()
    navigate({ pathname: path, search: search ? `?${search}` : '' })
  }

  return (
    <aside className="barca-sidebar">
      <div className="barca-brand">
        <img src={markUrl} width="22" height="22" alt="" />
        <span className="barca-wordmark">barca</span>
      </div>

      <div className="barca-sect">
        <span>Pipelines</span>
      </div>
      <div className="barca-pipes">
        {!nodes && <span className="barca-pipe-empty">loading…</span>}
        {nodes && pipelines.length === 0 && (
          <span className="barca-pipe-empty">no pipelines served</span>
        )}
        {pipelines.length > 1 && (
          <button
            className="barca-pipe"
            data-active={selected === null ? '' : undefined}
            onClick={() => select(null)}
          >
            <Layers size={12} />
            <span className="barca-pipe-name">All pipelines</span>
            <span className="barca-pipe-count">{nodes?.length}</span>
          </button>
        )}
        {pipelines.map((p) => (
          <button
            key={p.file}
            className="barca-pipe"
            title={p.file}
            data-active={selected === p.file ? '' : undefined}
            aria-pressed={selected === p.file}
            onClick={() => select(selected === p.file ? null : p.file)}
          >
            <StatusDot status={severityStatus(p.worst)} size={6} />
            <span className="barca-pipe-name">{p.name}</span>
            <span className="barca-pipe-count">{p.total}</span>
          </button>
        ))}
      </div>
      <div className="barca-side-foot"><Link className="barca-nav" to="/docs">Docs</Link></div>
    </aside>
  )
}
