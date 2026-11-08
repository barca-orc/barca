import { useEffect, useRef } from 'react'
import { useVirtualizer } from '@tanstack/react-virtual'
import { StatusGlyph } from '@/components/StatusGlyph'
import { formatAgo } from '@/lib/time'
import { formatBytes } from '@/lib/nodeDetail'
import { formatSeconds } from '@/lib/assetTable'
import {
  attemptOf, errorOf, KEY_STATE_GLYPH, KEY_STATE_WORD, stateOf, valuesOf, type Grid,
} from '@/lib/partitionGrid'

export interface KeyTableProps {
  grid: Grid
  /** The keys shown, in key order. */
  cells: readonly number[]
  selected: number | null
  onSelect: (cell: number) => void
  nowMs: number
}

const ROW = 30

/**
 * barca · KeyTable
 * The keys the filters keep, one line each and a column per dimension; only the lines in view
 * are rendered, so 10,000 keys scroll like ten.
 */
export function KeyTable({ grid, cells, selected, onSelect, nowMs }: KeyTableProps) {
  const scroll = useRef<HTMLDivElement>(null)
  // eslint-disable-next-line react-hooks/incompatible-library
  const virtualizer = useVirtualizer({
    count: cells.length,
    getScrollElement: () => scroll.current,
    estimateSize: () => ROW,
    overscan: 16,
  })
  const at = selected === null ? -1 : cells.indexOf(selected)
  useEffect(() => {
    if (at >= 0) virtualizer.scrollToIndex(at, { align: 'auto' })
  }, [at, virtualizer])
  const columns = `${grid.dims.map(() => 'minmax(90px, 1fr)').join(' ')} 104px 84px 64px 64px minmax(160px, 2fr)`
  return (
    <div className="barca-keytable" role="table" aria-label="Keys" aria-rowcount={cells.length + 1}>
      <div className="barca-keytable-head" role="row" style={{ gridTemplateColumns: columns }}>
        {grid.dims.map((d) => <span key={d.name} role="columnheader">{d.name}</span>)}
        <span role="columnheader">State</span>
        <span role="columnheader">Last attempt</span>
        <span role="columnheader" className="is-num">Took</span>
        <span role="columnheader" className="is-num">Size</span>
        <span role="columnheader">Error</span>
      </div>
      <div className="barca-keytable-body" ref={scroll}>
        {cells.length === 0 && <p className="barca-note barca-keytable-empty">No key matches.</p>}
        <div style={{ height: virtualizer.getTotalSize(), position: 'relative' }}>
          {virtualizer.getVirtualItems().map((item) => {
            const cell = cells[item.index]!
            const state = stateOf(grid, cell)
            const attempt = attemptOf(grid, cell)
            const error = errorOf(grid, cell)
            return (
              <div
                key={cell}
                role="row"
                aria-rowindex={item.index + 2}
                aria-selected={cell === selected}
                tabIndex={0}
                className="barca-keytable-row"
                style={{ gridTemplateColumns: columns, transform: `translateY(${item.start}px)`, height: ROW }}
                onClick={() => onSelect(cell)}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') onSelect(cell)
                  if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
                    e.preventDefault()
                    const next = cells[item.index + (e.key === 'ArrowDown' ? 1 : -1)]
                    if (next !== undefined) onSelect(next)
                  }
                }}
              >
                {valuesOf(grid, cell).map((v, d) => <code key={d} role="cell" title={v}>{v}</code>)}
                <span role="cell" className="barca-keytable-state">
                  {state && <><StatusGlyph glyph={KEY_STATE_GLYPH[state]} size={12} />{KEY_STATE_WORD[state]}</>}
                </span>
                <span role="cell" className="barca-cell-sub">{attempt?.at ? formatAgo(new Date(attempt.at * 1000).toISOString(), nowMs) : '–'}</span>
                <span role="cell" className="barca-cell-sub is-num">{attempt?.elapsedMs != null ? formatSeconds(attempt.elapsedMs / 1000) : '–'}</span>
                <span role="cell" className="barca-cell-sub is-num">{attempt?.sizeBytes != null ? formatBytes(attempt.sizeBytes) : '–'}</span>
                <span role="cell" className="barca-keytable-error" title={error?.text}>{error?.text ?? ''}</span>
              </div>
            )
          })}
        </div>
      </div>
    </div>
  )
}
