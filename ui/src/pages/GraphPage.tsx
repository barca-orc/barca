import { StreamNotice } from '@/components/StreamNotice'
import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { Link, useSearchParams } from 'react-router'
import { Maximize, ArrowRight, ArrowDown } from 'lucide-react'
import { IconButton, StatusDot, ViewToggle } from '@/components'
import { GraphCanvas, type GraphCanvasHandle } from '@/components/graph/GraphCanvas'
import { NodeInspector } from '@/components/graph/NodeInspector'
import { useAssets } from '@/hooks/useAssets'
import { useAssetStates } from '@/hooks/useAssetStates'
import { useQueryClient } from '@tanstack/react-query'
import { graphState, liveGraphState, PENDING_GRAPH_STATE, UNKNOWN_GRAPH_STATE } from '@/lib/graphState'
import { useHealth } from '@/hooks/useHealth'
import { useRunStream } from '@/hooks/useRunStream'
import { useTriggerNode } from '@/hooks/useTriggerNode'
import { useTopbarRun } from '@/hooks/useTopbarRun'
import { inPipeline, pipelineName } from '@/lib/pipeline'
import { overlayRunStatus, type LayoutDir } from '@/lib/graph'
import type { StatusKind } from '@/lib/types'

const LEGEND: { status: StatusKind; label: string }[] = [
  {status:'failed', label:'Failed'}, {status:'running', label:'Running'}, {status:'warning', label:'Stale'},
  {status:'skipped', label:'Not run'}, {status:'success', label:'Healthy'},
]

export function GraphPage() {
  const assetQuery = useAssets()
  const allAssets = useMemo(() => assetQuery.isPlaceholderData ? [] : assetQuery.data ?? [], [assetQuery.isPlaceholderData, assetQuery.data])
  const stateQuery = useAssetStates()
  const queryClient = useQueryClient()
  const { data: health } = useHealth()
  // `?focus=<node id>` (from the Assets table) opens with that node selected.
  const [searchParams, setSearchParams] = useSearchParams()
  const selected = searchParams.get('focus')
  const dir: LayoutDir = searchParams.get('dir') === 'TB' ? 'TB' : 'LR'
  const setSelected = (id: string | null) => {
    const next = new URLSearchParams(searchParams)
    if (id) next.set('focus', id)
    else next.delete('focus')
    setSearchParams(next, { replace: true })
  }
  // The pipeline picked in the sidebar (?pipeline=<file>): show only its nodes.
  const pipeline = searchParams.get('pipeline')
  const assets = useMemo(
    () => allAssets.filter((a) => inPipeline(a.id, pipeline)),
    [allAssets, pipeline],
  )
  const [run, setRun] = useState<{ handle: string; nodeId: string | null } | null>(null)
  const canvasRef = useRef<GraphCanvasHandle>(null)

  const stream = useRunStream(run?.handle ?? null)

  const title = pipeline ? `Assets · ${pipelineName(pipeline)}` : 'Assets'

  const selectedAsset = useMemo(
    () => assets.find((a) => a.id === selected) ?? null,
    [assets, selected],
  )

  const onTrigger = useCallback(
    (handle: string, nodeId: string) => setRun({ handle, nodeId }),
    [],
  )
  const trigger = useTriggerNode(selectedAsset, onTrigger)
  const readOnly = health?.read_only ?? false

  // The topbar's Run button acts on the selected node, same as the inspector's, else on what
  // is open.
  const onStarted = useCallback((handle: string) => setRun({ handle, nodeId: null }), [])
  const topbar = useTopbarRun({
    node: selectedAsset, show: true, scopes: true, fireNode: trigger.fire,
    busy: trigger.isPending || stream.running, onStarted,
  })

  // Live status overlay: stream-derived, plus an optimistic "running" on the
  // triggered node so the click feels instant before the first event lands.
  const statuses = useMemo(
    () => overlayRunStatus(stream.statuses, stream.running, run?.nodeId ?? null),
    [stream.statuses, stream.running, run],
  )

  const visualStates = useMemo(() => {
    const base = Object.fromEntries(assets.map(a => [a.id, stateQuery.isError ? UNKNOWN_GRAPH_STATE : PENDING_GRAPH_STATE]))
    if (!stateQuery.isError) for (const node of stateQuery.data ?? []) base[node.id] = graphState(node)
    if (stream.running) for (const [id, status] of Object.entries(statuses)) base[id] = liveGraphState(status)
    return base
  }, [assets, stateQuery.data, stateQuery.isError, statuses, stream.running])
  useEffect(() => {
    if (run && !stream.running) void queryClient.invalidateQueries({queryKey:['state']})
  }, [run, stream.running, queryClient])
  const selectedState = selected ? visualStates[selected] ?? UNKNOWN_GRAPH_STATE : UNKNOWN_GRAPH_STATE
  const selectedError = (selected && stream.running && stream.errors[selected]) || stateQuery.data?.find(n => n.id === selected)?.last_materialization?.error || null

  return (
    <div className="barca-view">
      <StreamNotice notice={stream.notice} runId={run?.handle} />
      <div className="barca-view-head">
        <div className="barca-view-bar">
          <div className="barca-view-title">
            <h1>{title}</h1>
          </div>
          <div className="barca-view-actions">
            {run && <Link className="barca-clear" to={`/runs?run=${encodeURIComponent(run.handle)}`}>View run</Link>}
            <ViewToggle />
            <IconButton
              label={dir === 'LR' ? 'Top-down layout' : 'Left-right layout'}
              onClick={() => { const next = new URLSearchParams(searchParams); if (dir === 'LR') next.set('dir', 'TB'); else next.delete('dir'); setSearchParams(next, { replace: true }) }}
            >
              {dir === 'LR' ? <ArrowDown size={15} /> : <ArrowRight size={15} />}
            </IconButton>
            <IconButton label="Fit to screen" onClick={() => canvasRef.current?.fit()}>
              <Maximize size={15} />
            </IconButton>
          </div>
        </div>
      </div>

      {topbar.error && <p role="alert">Could not start run: {topbar.error.message}</p>}
      {stateQuery.isError && <div className="barca-graph-tools" role="status">Asset state unavailable; colors are neutral.</div>}
      <div className="barca-graph-wrap">
        <div className="barca-graph-canvas">
          <GraphCanvas
            assets={assets}
            dir={dir}
            selected={selected}
            onSelect={setSelected}
            states={visualStates}
            handleRef={canvasRef}
          />
          <div className="barca-legend">
            {LEGEND.map((s) => (
              <span key={s.label}>
                <StatusDot status={s.status} size={6} />
                {s.label}
              </span>
            ))}
          </div>
        </div>
        {selectedAsset && (
          <NodeInspector
            asset={selectedAsset}
            status={selectedState.status}
            statusLabel={selectedState.label}
            feedbackStatus={statuses[selectedAsset.id] ?? (selectedState.status === 'failed' ? 'failed' : 'skipped')}
            logs={stream.logs}
            running={stream.running}
            error={selectedError}
            readOnly={readOnly}
            triggering={trigger.isPending}
            triggerError={trigger.error}
            onFire={trigger.fire}
            onClose={() => setSelected(null)}
          />
        )}
      </div>
    </div>
  )
}
