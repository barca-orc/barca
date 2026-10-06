import { useMemo, useRef, useState } from 'react'
import { useSearchParams } from 'react-router'
import { Filter, Maximize, ArrowRight, ArrowDown } from 'lucide-react'
import { IconButton, StatusDot } from '@/components'
import { GraphCanvas, type GraphCanvasHandle } from '@/components/graph/GraphCanvas'
import { NodeInspector } from '@/components/graph/NodeInspector'
import { useAssets } from '@/hooks/useAssets'
import { useHealth } from '@/hooks/useHealth'
import { useRunStream } from '@/hooks/useRunStream'
import { sourceFile, pipelineName } from '@/lib/pipeline'
import { overlayRunStatus, type LayoutDir } from '@/lib/graph'
import type { StatusKind } from '@/lib/types'

const LEGEND: StatusKind[] = ['success', 'running', 'queued', 'failed']

export function GraphPage() {
  const { data: assets = [], isError } = useAssets()
  const { data: health } = useHealth()
  const [dir, setDir] = useState<LayoutDir>('LR')
  // `?focus=<node id>` (from the Assets table) opens with that node selected.
  const [searchParams] = useSearchParams()
  const [selected, setSelected] = useState<string | null>(searchParams.get('focus'))
  const [run, setRun] = useState<{ handle: string; nodeId: string } | null>(null)
  const canvasRef = useRef<GraphCanvasHandle>(null)

  const stream = useRunStream(run?.handle ?? null)

  const connected = !!health && !isError
  const title = assets[0] ? pipelineName(sourceFile(assets[0].id)) : 'graph'

  const selectedAsset = useMemo(
    () => assets.find((a) => a.id === selected) ?? null,
    [assets, selected],
  )

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
            <span className="barca-conn">
              <StatusDot status={connected ? 'success' : 'queued'} size={6} />
              {connected ? `barca serve · v${health.version}` : 'offline · mock data'}
            </span>
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
            readOnly={health?.read_only ?? false}
            onTrigger={(handle, nodeId) => setRun({ handle, nodeId })}
            onClose={() => setSelected(null)}
          />
        )}
      </div>
    </div>
  )
}
