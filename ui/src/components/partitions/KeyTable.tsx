import { useEffect, useRef } from 'react'
import { useVirtualizer } from '@tanstack/react-virtual'
import { StatusGlyph } from '@/components/StatusGlyph'
import { formatAgo, formatSeconds } from '@/lib/time'
import { formatBytes } from '@/lib/nodeDetail'
import { keyStateDisplay, keyStateOf } from '@/lib/keyStateDisplay'
import type { KeySort } from '@/lib/keysPage'
import type { KeyRow } from '@/lib/types'

export interface KeyTableProps {
  /** The dimensions' names, a column each. */
  dimensions: readonly string[]
  /** The keys loaded so far, in the server's order. */
  rows: readonly KeyRow[]
  /** How many keys the server says match; `rows` may hold fewer. */
  total: number
  /** The open key, as the server spells it. */
  selected: string | null
  onSelect: (key: string) => void
  /** The table was scrolled near the end of what is loaded: ask for the next page. */
  onNeedMore: () => void
  loadingMore: boolean
  sort: KeySort
  onSort: (sort: KeySort) => void
  /** What the server was asked for (filters and order): a new one starts the list at its top. */
  query: string
  /** `rows` are still the previous query's, shown until this one's answer arrives. */
  stale: boolean
  nowMs: number
}

const ROW = 30

/** How close to the end of the loaded rows the view gets before the next page is asked for. */
const AHEAD = 40

function SortHeader({ label, order, sort, onSort, num }: { label: string; order: KeySort; sort: KeySort; onSort: (sort: KeySort) => void; num?: boolean }) {
  const active = sort === order
  return (
    <span role="columnheader" className={num ? 'is-num' : undefined} aria-sort={active ? 'descending' : undefined}>
      <button type="button" className="barca-keytable-sort" aria-pressed={active} title={active ? 'Back to key order' : `Order by ${label.toLowerCase()}`} onClick={() => onSort(order)}>
        {label}{active && <span aria-hidden> ↓</span>}
      </button>
    </span>
  )
}

/**
 * barca · KeyTable
 * The keys the server lists for the page's filters, one line each and a column per dimension.
 * Only the lines in view are rendered and only the pages scrolled to are loaded, so 100,000
 * keys scroll like ten. The order is the server's: a column header asks for another.
 */
export function KeyTable({ dimensions, rows, total, selected, onSelect, onNeedMore, loadingMore, sort, onSort, query, stale, nowMs }: KeyTableProps) {
  const scroll = useRef<HTMLDivElement>(null)
  // eslint-disable-next-line react-hooks/incompatible-library
  const virtualizer = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scroll.current,
    estimateSize: () => ROW,
    overscan: 16,
  })
  // Another filter or order is another list: it starts at its top, not where the last one was scrolled to.
  useEffect(() => {
    scroll.current?.scrollTo({ top: 0 })
  }, [query])
  // The open key is brought into view once this query's own rows are here.
  const at = selected === null || stale ? -1 : rows.findIndex((r) => r.key === selected)
  useEffect(() => {
    if (at >= 0) virtualizer.scrollToIndex(at, { align: 'auto' })
  }, [at, virtualizer])
  const items = virtualizer.getVirtualItems()
  const lastShown = items[items.length - 1]?.index ?? -1
  const more = rows.length < total
  useEffect(() => {
    if (more && !stale && !loadingMore && lastShown >= rows.length - AHEAD) onNeedMore()
  }, [more, stale, loadingMore, lastShown, rows.length, onNeedMore])
  const columns = `${dimensions.map(() => 'minmax(90px, 1fr)').join(' ')} 104px 96px 64px 64px 64px minmax(160px, 2fr)`
  return (
    <div className="barca-keytable" role="table" aria-label="Keys" aria-rowcount={total + 1}>
      <div className="barca-keytable-head" role="row" style={{ gridTemplateColumns: columns }}>
        {dimensions.map((d) => <span key={d} role="columnheader">{d}</span>)}
        <SortHeader label="State" order="state" sort={sort} onSort={onSort} />
        <SortHeader label="Last attempt" order="attempted" sort={sort} onSort={onSort} />
        <SortHeader label="Took" order="elapsed" sort={sort} onSort={onSort} num />
        <SortHeader label="Size" order="size" sort={sort} onSort={onSort} num />
        <SortHeader label="Rows" order="rows" sort={sort} onSort={onSort} num />
        <span role="columnheader">Error</span>
      </div>
      <div className="barca-keytable-body" ref={scroll}>
        {total === 0 && <p className="barca-note barca-keytable-empty">No key matches.</p>}
        <div style={{ height: virtualizer.getTotalSize() + (more ? ROW : 0), position: 'relative' }}>
          {items.map((item) => {
            const row = rows[item.index]!
            const state = keyStateOf(row.state)
            const shown = state ? keyStateDisplay(state) : null
            return (
              <div
                key={row.key}
                role="row"
                aria-rowindex={item.index + 2}
                aria-selected={row.key === selected}
                tabIndex={0}
                className="barca-keytable-row"
                style={{ gridTemplateColumns: columns, transform: `translateY(${item.start}px)`, height: ROW }}
                onClick={() => onSelect(row.key)}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') onSelect(row.key)
                  if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
                    e.preventDefault()
                    const next = rows[item.index + (e.key === 'ArrowDown' ? 1 : -1)]
                    if (next !== undefined) onSelect(next.key)
                  }
                }}
              >
                {row.values.map((v, d) => <code key={d} role="cell" title={v}>{v}</code>)}
                <span role="cell" className="barca-keytable-state">
                  {shown ? <><StatusGlyph glyph={shown.glyph} size={12} />{shown.word}</> : row.state}
                </span>
                <span role="cell" className="barca-cell-sub">{row.attempted_at ? formatAgo(row.attempted_at, nowMs) : '–'}</span>
                <span role="cell" className="barca-cell-sub is-num">{row.elapsed_seconds != null ? formatSeconds(row.elapsed_seconds) : '–'}</span>
                <span role="cell" className="barca-cell-sub is-num">{row.size_bytes != null ? formatBytes(row.size_bytes) : '–'}</span>
                <span role="cell" className="barca-cell-sub is-num">{row.rows == null ? '–' : row.rows.toLocaleString()}</span>
                <span role="cell" className="barca-keytable-error" title={row.error ?? undefined}>{row.error ?? ''}</span>
              </div>
            )
          })}
          {more && (
            <p className="barca-cell-sub barca-keytable-more" style={{ transform: `translateY(${virtualizer.getTotalSize()}px)`, height: ROW }}>
              {loadingMore ? 'Loading more keys…' : `${(total - rows.length).toLocaleString()} more`}
            </p>
          )}
        </div>
      </div>
    </div>
  )
}
