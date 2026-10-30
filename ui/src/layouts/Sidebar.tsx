import { Link, useLocation, useNavigate, useSearchParams } from 'react-router'
import { useMemo } from 'react'
import { Layers } from 'lucide-react'
import { StatusDot } from '@/components'
import { useAssetStates, useWholeState } from '@/hooks/useAssetStates'
import { useAssets } from '@/hooks/useAssets'
import { severityStatus } from '@/lib/assetTable'
import { severityWord } from '@/lib/status'
import { mergeStates, pipelineSummaries } from '@/lib/pipeline'
import { authMode, connect } from '@/lib/credentials'
import markUrl from '@/assets/brand/mark.svg'

/** Pages that can show one pipeline; elsewhere a pipeline click opens Assets. */
const FILTERABLE = ['/assets', '/graph', '/tasks', '/sensors', '/schedules']

export function Sidebar() {
  const { data: scoped } = useAssetStates()
  const { data: whole } = useWholeState()
  const assetQuery = useAssets()
  const assets = assetQuery.isPlaceholderData ? undefined : assetQuery.data
  // The open scope is fresh; the pipelines outside it keep what the whole project last said.
  const nodes = useMemo(() => mergeStates(whole, scoped), [whole, scoped])
  const pipelines = useMemo(() => pipelineSummaries(nodes, assets ?? []), [nodes, assets])
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
        {!assets && <span className="barca-pipe-empty">loading…</span>}
        {assets && pipelines.length === 0 && (
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
            <span className="barca-pipe-count">{assets?.length}</span>
          </button>
        )}
        {pipelines.map((p) => (
          <button
            key={p.file}
            className="barca-pipe"
            title={p.known ? `${p.file} · ${severityWord(p.worst)}` : p.file}
            data-active={selected === p.file ? '' : undefined}
            aria-pressed={selected === p.file}
            onClick={() => select(selected === p.file ? null : p.file)}
          >
            {/* A dot says the whole file's state: none until all of it is known. */}
            {p.running || p.known
              ? <StatusDot status={p.running ? 'running' : severityStatus(p.worst)} size={6} />
              : <span className="barca-pipe-nodot" />}
            <span className="barca-pipe-name">{p.name}</span>
            {p.running > 0 && (
              <span className="barca-pipe-running" title={`${p.running} running now`}>
                {p.running} running
              </span>
            )}
            <span className="barca-pipe-count">{p.total}</span>
          </button>
        ))}
      </div>
      <div className="barca-side-foot">
        <Link className="barca-nav" to="/docs">Docs</Link>
        {authMode() === 'token' && <><Link className="barca-nav" to="/tokens">API tokens</Link><button className="barca-nav" onClick={() => connect(null)}>Disconnect</button></>}
      </div>
    </aside>
  )
}
