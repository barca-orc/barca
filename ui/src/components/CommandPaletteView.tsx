import { useEffect, useRef } from 'react'
import type { PaletteItem } from '@/lib/palette'


export interface CommandPaletteViewProps {
  query: string
  items: readonly PaletteItem[]
  active: number
  /** Partitioned nodes are still answering the key query. */
  searching?: boolean
  onQuery: (value: string) => void
  onMove: (by: number) => void
  onPick: (item: PaletteItem) => void
  onClose: () => void
}

/** barca · CommandPalette: one field that reaches any node, group, file, run or partition key. */
export function CommandPaletteView({ query, items, active, searching = false, onQuery, onMove, onPick, onClose }: CommandPaletteViewProps) {
  const list = useRef<HTMLUListElement>(null)
  useEffect(() => {
    list.current?.children[active]?.scrollIntoView({ block: 'nearest' })
  }, [active])
  return (
    <div className="barca-palette-backdrop" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div className="barca-palette" role="dialog" aria-modal="true" aria-label="Command palette">
        <input
          autoFocus
          role="combobox"
          aria-expanded="true"
          aria-controls="barca-palette-list"
          aria-label="Jump to a node, group, file, run or partition key"
          placeholder="Jump to a node, group, file, run or partition key"
          value={query}
          onChange={(e) => onQuery(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
              e.preventDefault()
              onMove(e.key === 'ArrowDown' ? 1 : -1)
            } else if (e.key === 'Enter') {
              const item = items[active]
              if (item) onPick(item)
            } else if (e.key === 'Escape') {
              e.stopPropagation()
              onClose()
            }
          }}
        />
        {items.length === 0 ? <p className="barca-palette-empty">{searching ? 'Searching keys…' : 'Nothing matches.'}</p> : (
          <ul id="barca-palette-list" role="listbox" ref={list} aria-label="Results">
            {items.map((item, i) => (
              <li key={item.key} role="option" aria-selected={i === active} onClick={() => onPick(item)}>
                <b title={item.label}>{item.label}</b>
                <span>{item.detail}</span>
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  )
}
