import { useLayoutEffect, useMemo, useRef, useState } from 'react'
import { heatmap, KEY_STATE_WORD, KEY_STATES, type Grid, type GridLayout, type HeatBin } from '@/lib/partitionGrid'

export interface KeyHeatmapProps {
  grid: Grid
  layout: GridLayout
  /** The stretch of the long dimension drawn (index bounds), or every value. */
  window: [number, number] | null
  /** A stretch was dragged across (or one column clicked): zoom to it. */
  onZoom: (from: number, to: number) => void
  /** A column of one key was clicked: open it. */
  onOpen: (cell: number) => void
}

const LABEL_W = 72
const MIN_COL = 6

/** A column where only a few of its keys need attention: drawn as a mark, not a solid block. */
function someOnly(bin: HeatBin): boolean {
  if (!bin.worst || bin.worst === 'fresh') return false
  const total = KEY_STATES.reduce((n, s) => n + bin.counts[s], 0)
  return bin.counts.fresh / total > 0.5
}

function binTitle(bin: HeatBin, values: readonly string[], row: string): string {
  const where = bin.from === bin.to ? values[bin.from] : `${values[bin.from]} → ${values[bin.to]}`
  const counts = KEY_STATES.filter((s) => bin.counts[s] > 0).map((s) => `${bin.counts[s].toLocaleString()} ${KEY_STATE_WORD[s]}`).join(', ')
  return `${row ? `${row} · ` : ''}${where}: ${counts}`
}

/**
 * barca · KeyHeatmap
 * Every key of a partitioned asset in a few rows: one per value of its shorter dimensions, its
 * longest cut into as many columns as fit, each column drawn in the most urgent state of its
 * keys. Fresh keys are quiet and problems are loud, so where things went wrong reads at a glance;
 * dragging across columns zooms to them, down to one key per column.
 */
export function KeyHeatmap({ grid, layout, window, onZoom, onOpen }: KeyHeatmapProps) {
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
  const showLabels = layout.blocks.length > 1
  const columns = Math.max(8, Math.floor((width - (showLabels ? LABEL_W : 0)) / MIN_COL))
  // A label every 130px or so, so that dates never run into each other.
  const ticks = Math.max(2, Math.floor((width - (showLabels ? LABEL_W : 0)) / 130))
  const map = useMemo(() => heatmap(grid, layout, columns, window, ticks), [grid, layout, columns, window, ticks])
  const values = grid.dims[map.along]?.values ?? []
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
        {map.rows.map((row) => (
          <div key={row.label} className="barca-heat-row" role="row" aria-label={row.label || 'keys'}>
            {showLabels && <span className="barca-heat-label" title={row.label}>{row.label}</span>}
            {row.bins.map((bin, c) => {
              const one = bin.from === bin.to
              const cell = one ? layout.blocks[map.rows.indexOf(row)]!.cells[bin.from]! : null
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
                    if (drag && drag.start === c && cell !== null) {
                      setDrag(null)
                      onOpen(cell)
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
