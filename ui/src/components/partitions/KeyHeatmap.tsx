import { memo, useLayoutEffect, useMemo, useRef, useState } from 'react'
import { KEY_STATES, keyStateDisplay, QUIET_KEY_STATE } from '@/lib/keyStateDisplay'
import { alongValues, cellKey, heatmap, type HeatBin } from '@/lib/partitionGrid'
import type { KeysOverview } from '@/lib/types'

export interface KeyHeatmapProps {
  /** The served rows of the node's keys (`GET /assets/{id}/partitions/overview`). */
  overview: KeysOverview
  /** The stretch of the long dimension drawn (index bounds), or every value. */
  window: [number, number] | null
  /** A stretch was dragged across (or one column clicked): zoom to it. */
  onZoom: (from: number, to: number) => void
  /** A column of one key was clicked: open it. */
  onOpen: (key: string) => void
}

const LABEL_W = 72
const MIN_COL = 6

/** A column where only a few of its keys need attention: drawn as a mark, not a solid block. */
function someOnly(bin: HeatBin): boolean {
  if (!bin.worst || bin.worst === QUIET_KEY_STATE) return false
  const total = KEY_STATES.reduce((n, s) => n + bin.counts[s], 0)
  return bin.counts[QUIET_KEY_STATE] / total > 0.5
}

function binTitle(bin: HeatBin, values: readonly string[], row: string): string {
  const where = bin.from === bin.to ? values[bin.from] : `${values[bin.from]} → ${values[bin.to]}`
  const counts = KEY_STATES.filter((s) => bin.counts[s] > 0).map((s) => `${bin.counts[s].toLocaleString()} ${keyStateDisplay(s).word}`).join(', ')
  return `${row ? `${row} · ` : ''}${where}: ${counts}`
}

/**
 * barca · KeyHeatmap
 * Every key of a partitioned asset in a few rows: one per value of its shorter dimensions (the
 * server's rows), its longest cut into as many columns as fit, each column drawn in the most
 * urgent state of its keys. Fresh keys are quiet and problems are loud, so where things went
 * wrong reads at a glance; dragging across columns zooms to them, down to one key per column.
 *
 * Drawn again only when the served rows or the zoom change: a node of 100,000 keys is tens of
 * thousands of cells, and a filter typed into the table changes neither. The callbacks act on
 * the page's address as it is when they are called, so an older pair is as good as a new one.
 */
export const KeyHeatmap = memo(KeyHeatmapView, (a, b) => a.overview === b.overview && a.window?.[0] === b.window?.[0] && a.window?.[1] === b.window?.[1])

function KeyHeatmapView({ overview, window, onZoom, onOpen }: KeyHeatmapProps) {
  const box = useRef<HTMLDivElement>(null)
  const [width, setWidth] = useState(0)
  const [drag, setDrag] = useState<{ start: number; end: number } | null>(null)
  useLayoutEffect(() => {
    const el = box.current
    if (!el) return
    const observer = new ResizeObserver(([entry]) => setWidth(Math.floor(entry!.contentRect.width)))
    observer.observe(el)
    setWidth(Math.floor(el.getBoundingClientRect().width))
    return () => observer.disconnect()
  }, [])
  const showLabels = overview.rows.length > 1
  const columns = Math.max(8, Math.floor((width - (showLabels ? LABEL_W : 0)) / MIN_COL))
  // A label every 130px or so, so that dates never run into each other.
  const ticks = Math.max(2, Math.floor((width - (showLabels ? LABEL_W : 0)) / 130))
  const map = useMemo(() => heatmap(overview, columns, window, ticks), [overview, columns, window, ticks])
  const values = alongValues(overview)
  const cols = map.rows[0]?.bins.length ?? 0
  const [lo, hi] = drag ? [Math.min(drag.start, drag.end), Math.max(drag.start, drag.end)] : [-1, -1]

  const finish = (col: number) => {
    if (!drag) return
    const a = Math.min(drag.start, col)
    const b = Math.max(drag.start, col)
    setDrag(null)
    const first = map.rows[0]!.bins[a]!
    const last = map.rows[0]!.bins[b]!
    if (a === b && first.from === first.to) return
    onZoom(first.from, last.to)
  }

  return (
    <div className="barca-heat" ref={box} onMouseLeave={() => setDrag(null)}>
      <div className="barca-heat-rows" style={{ gridTemplateColumns: `${showLabels ? `${LABEL_W}px ` : ''}repeat(${cols}, 1fr)` }}>
        {map.rows.map((row, r) => (
          <div key={row.label} className="barca-heat-row" role="row" aria-label={row.label || 'keys'}>
            {showLabels && <span className="barca-heat-label" title={row.label}>{row.label}</span>}
            {row.bins.map((bin, c) => {
              // A column of one value that holds a key opens it.
              const key = bin.from === bin.to && bin.worst ? cellKey(overview, overview.rows[r]!, bin.from) : null
              return (
                <span
                  key={c}
                  className="barca-heat-bin"
                  data-state={bin.worst ?? undefined}
                  data-some={someOnly(bin) || undefined}
                  data-picked={(c >= lo && c <= hi) || undefined}
                  title={bin.worst ? binTitle(bin, values, row.label) : undefined}
                  onMouseDown={(e) => { e.preventDefault(); setDrag({ start: c, end: c }) }}
                  onMouseEnter={() => drag && setDrag({ ...drag, end: c })}
                  onMouseUp={() => {
                    if (drag && drag.start === c && key !== null) {
                      setDrag(null)
                      onOpen(key)
                    } else finish(c)
                  }}
                />
              )
            })}
          </div>
        ))}
      </div>
      <div className="barca-heat-axis" style={{ marginLeft: showLabels ? LABEL_W : 0 }}>
        {map.ticks.map((t) => (
          <span key={t.bin} style={{ left: `${(100 * t.bin) / Math.max(1, cols)}%` }}>{t.label}</span>
        ))}
      </div>
    </div>
  )
}
