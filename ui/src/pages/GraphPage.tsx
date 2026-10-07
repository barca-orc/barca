import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { useOutletContext, useSearchParams } from 'react-router'
import { Filter, Maximize, ArrowRight, ArrowDown } from 'lucide-react'
import { ConnectionBadge, IconButton, StatusDot } from '@/components'
import { GraphCanvas, type GraphCanvasHandle } from '@/components/graph/GraphCanvas'
import { NodeInspector } from '@/components/graph/NodeInspector'
import { useAssets } from '@/hooks/useAssets'
import { useHealth } from '@/hooks/useHealth'
import { connection } from '@/lib/connection'
import { useRunStream } from '@/hooks/useRunStream'
import { useTriggerNode } from '@/hooks/useTriggerNode'
import type { AppShellContext } from '@/layouts/shellContext'
import { shortName } from '@/lib/graph'
import { inPipeline, sourceFile, pipelineName } from '@/lib/pipeline'
import { overlayRunStatus, type LayoutDir } from '@/lib/graph'
import type { StatusKind } from '@/lib/types'

const LEGEND: StatusKind[] = ['success', 'running', 'queued', 'failed']

export function GraphPage() {
  const { data: allAssets = [] } = useAssets()
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

  const selectedStatus = (selected && statuses[selected]) || 'queued'
  const selectedError = (selected && stream.errors[selected]) || null

  return (
    <div className="barca-view">
      <div className="barca-view-head">
        <div className="barca-view-bar">
          <div className="barca-view-title">
            <h1>{title}</h1>
          </div>
          <div className="barca-view-actions">
            <ConnectionBadge
              connection={connection(health, healthError)}
              offlineLabel="offline · mock data"
            />
            <IconButton
              label={dir === 'LR' ? 'Top-down layout' : 'Left-right layout'}
              onClick={() => setDir((d) => (d === 'LR' ? 'TB' : 'LR'))}
            >
              {dir === 'LR' ? <ArrowDown size={15} /> : <ArrowRight size={15} />}
            </IconButton>
            <IconButton label="Filter">
              <Filter size={15} />
            </IconButton>
            <IconButton label="Fit to screen" onClick={() => canvasRef.current?.fit()}>
              <Maximize size={15} />
            </IconButton>
          </div>
        </div>
      </div>

      <div className="barca-graph-wrap">
        <div className="barca-graph-canvas">
          <GraphCanvas
            assets={assets}
            dir={dir}
            selected={selected}
            onSelect={setSelected}
            statuses={statuses}
            handleRef={canvasRef}
          />
          <div className="barca-legend">
            {LEGEND.map((s) => (
              <span key={s}>
                <StatusDot status={s} size={6} />
                {s}
              </span>
            ))}
          </div>
        </div>
        {selectedAsset && (
          <NodeInspector
            asset={selectedAsset}
            status={selectedStatus}
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
