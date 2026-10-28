import { useCallback, type RefObject } from 'react'
import { useVirtualizer, type VirtualItem } from '@tanstack/react-virtual'

/** What `useTableWindow` gives a table body: the rows to render and the space around them. */
export interface TableWindow {
  /** The rows in (or near) view, in order; `index` is the row's index in the full list. */
  items: VirtualItem[]
  /** Height of the rows above the first rendered one (px); 0 when none are left out. */
  before: number
  /** Height of the rows below the last rendered one (px). */
  after: number
  /** Ref for each rendered `<tr>` (with `data-index={item.index}`), so its height is known. */
  measure: (row: HTMLTableRowElement | null) => void
  /** Scroll the least needed to bring row `index` into view. */
  scrollToIndex: (index: number) => void
}

/** Rows rendered beyond each edge of the view: enough for a keypress or a fast scroll. */
const OVERSCAN = 12

/**
 * barca · useTableWindow
 * Render only the rows of a long table that are in view. A table of a thousand nodes renders
 * every row on each `/state` poll otherwise, which held the main thread for hundreds of
 * milliseconds per poll. The rows keep their table layout: the rows left out are stood in for
 * by a `TableSpacer` (components/TableWindow.tsx) above and below, so the scrollbar and the sticky header behave as before.
 *
 * `estimate` is a typical row height; rows are measured as they render (one with an error
 * line is taller). The first render shows the top rows already, as many as fit the window,
 * so the table does not appear a frame after its data.
 */
export function useTableWindow(
  count: number,
  scrollRef: RefObject<HTMLElement | null>,
  estimate = 52,
): TableWindow {
  // Not memoized, as the lint notes: its functions are read while rendering, each render.
  // eslint-disable-next-line react-hooks/incompatible-library
  const virtualizer = useVirtualizer({
    count,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => estimate,
    overscan: OVERSCAN,
    initialRect: { width: 0, height: typeof window === 'undefined' ? 800 : window.innerHeight },
  })
  const items = virtualizer.getVirtualItems()
  const first = items[0]
  const last = items[items.length - 1]
  const before = first ? first.start : 0
  const after = last ? Math.max(0, virtualizer.getTotalSize() - last.end) : 0
  const scrollToIndex = useCallback(
    (index: number) => virtualizer.scrollToIndex(index, { align: 'auto' }),
    [virtualizer],
  )
  return { items, before, after, measure: virtualizer.measureElement, scrollToIndex }
}

/**
 * Focus row `index` of a `useTableWindow` table body and bring it into view. The row next to
 * the focused one is rendered already; one further away is scrolled to first, then focused.
 */
export function focusWindowRow(
  body: HTMLTableSectionElement | null,
  index: number,
  scrollToIndex: (index: number) => void,
) {
  const find = () => body?.querySelector<HTMLTableRowElement>(`tr[data-index="${index}"]`)
  const row = find()
  if (row) {
    row.focus({ preventScroll: true })
    row.scrollIntoView({ block: 'nearest' })
    return
  }
  scrollToIndex(index)
  requestAnimationFrame(() => find()?.focus({ preventScroll: true }))
}
