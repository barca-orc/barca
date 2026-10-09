import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { Link, useOutletContext, useSearchParams } from 'react-router'
import { Maximize, ArrowRight, ArrowDown } from 'lucide-react'
import { ConnectionBadge, IconButton, StatusDot } from '@/components'
import { GraphCanvas, type GraphCanvasHandle } from '@/components/graph/GraphCanvas'
import { NodeInspector } from '@/components/graph/NodeInspector'
import { useAssets } from '@/hooks/useAssets'
import { useAssetStates } from '@/hooks/useAssetStates'
import { useQueryClient } from '@tanstack/react-query'
import { graphState, liveGraphState, UNKNOWN_GRAPH_STATE } from '@/lib/graphState'
import { useHealth } from '@/hooks/useHealth'
import { connection } from '@/lib/connection'
import { useRunStream } from '@/hooks/useRunStream'
import { useTriggerNode } from '@/hooks/useTriggerNode'
import type { AppShellContext } from '@/layouts/shellContext'
import { shortName } from '@/lib/graph'
import { inPipeline, sourceFile, pipelineName } from '@/lib/pipeline'
import { overlayRunStatus, type LayoutDir } from '@/lib/graph'
import type { StatusKind } from '@/lib/types'

const LEGEND: { status: StatusKind; label: string }[] = [
  {status:'success', label:'Cached'}, {status:'warning', label:'Stale / partial'},
  {status:'skipped', label:'Neutral / no cached state'}, {status:'failed', label:'Last attempt failed'}, {status:'running', label:'Running'},
]

export function GraphPage() {
  const assetQuery = useAssets()
  const allAssets = useMemo(() => assetQuery.isPlaceholderData ? [] : assetQuery.data ?? [], [assetQuery.isPlaceholderData, assetQuery.data])
  const stateQuery = useAssetStates()
  const queryClient = useQueryClient()
  const { data: health, isError: healthError } = useHealth()
  const [dir, setDir] = useState<LayoutDir>('LR')
  // `?focus=<node id>` (from the Assets table) opens with that node selected.
  const [searchParams] = useSearchParams()
  const [selected, setSelected] = useState<string | null>(searchParams.get('focus'))
  // The pipeline picked in the sidebar (?pipeline=<file>): show only its nodes.
  const pipeline = searchParams.get('pipeline')
  const assets = useMemo(
    () => allAssets.filter((a) => inPipeline(a.id, pipeline)),
    [allAssets, pipeline],
  )
  const [run, setRun] = useState<{ handle: string; nodeId: string } | null>(null)
  const canvasRef = useRef<GraphCanvasHandle>(null)

  const stream = useRunStream(run?.handle ?? null)
  const { setTopbarRun } = useOutletContext<AppShellContext>()

  const title = pipeline
    ? pipelineName(pipeline)
    : assets[0] && assets.every((a) => sourceFile(a.id) === sourceFile(assets[0]!.id))
      ? pipelineName(sourceFile(assets[0].id))
      : 'All pipelines'

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

  // The topbar's Run button acts on the selected node, same as the inspector's.
  const { fire, verb } = trigger
  const triggering = trigger.isPending || stream.running
  const selectedName = selectedAsset ? shortName(selectedAsset.id) : null
  useEffect(() => {
    setTopbarRun({
      onRun: fire,
      disabled: !selectedName || readOnly,
      loading: triggering,
      title: readOnly
        ? 'This server is read-only'
        : selectedName
          ? `${verb} ${selectedName}`
          : 'Select a node to run',
    })
    return () => setTopbarRun(null)
  }, [setTopbarRun, fire, verb, selectedName, readOnly, triggering])

  // Live status overlay: stream-derived, plus an optimistic "running" on the
  // triggered node so the click feels instant before the first event lands.
  const statuses = useMemo(
    () => overlayRunStatus(stream.statuses, stream.running, run?.nodeId ?? null),
    [stream.statuses, stream.running, run],
  )

  const visualStates = useMemo(() => {
    const base = Object.fromEntries((stateQuery.isError ? [] : stateQuery.data ?? []).map(n => [n.id, graphState(n)]))
    if (stream.running) for (const [id, status] of Object.entries(statuses)) base[id] = liveGraphState(status)
    return base
  }, [stateQuery.data, stateQuery.isError, statuses, stream.running])
  useEffect(() => {
    if (run && !stream.running) void queryClient.invalidateQueries({queryKey:['state']})
  }, [run, stream.running, queryClient])
  const selectedState = selected ? visualStates[selected] ?? UNKNOWN_GRAPH_STATE : UNKNOWN_GRAPH_STATE
  const selectedError = (selected && stream.running && stream.errors[selected]) || stateQuery.data?.find(n => n.id === selected)?.last_materialization?.error || null

  return (
    <div className="barca-view">
      <div className="barca-view-head">
        <div className="barca-view-bar">
          <div className="barca-view-title">
            <h1>{title}</h1>
          </div>
          <div className="barca-view-actions">
            {run && <Link className="barca-clear" to={`/runs?run=${encodeURIComponent(run.handle)}`}>View run</Link>}
            <ConnectionBadge
              connection={connection(health, healthError)}
              offlineLabel="offline"
            />
            <IconButton
              label={dir === 'LR' ? 'Top-down layout' : 'Left-right layout'}
              onClick={() => setDir((d) => (d === 'LR' ? 'TB' : 'LR'))}
            >
              {dir === 'LR' ? <ArrowDown size={15} /> : <ArrowRight size={15} />}
            </IconButton>
            <IconButton label="Fit to screen" onClick={() => canvasRef.current?.fit()}>
              <Maximize size={15} />
            </IconButton>
          </div>
        </div>
      </div>

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
            verb={trigger.verb}
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
