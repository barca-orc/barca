import { useEffect, useRef } from 'react'
import { injectStyles } from '@/lib/injectStyles'
import type { PaletteItem } from '@/lib/palette'

const STYLE_ID = 'barca-palette-styles'
const CSS = `
  .barca-palette-backdrop { position: fixed; inset: 0; z-index: 60; display: flex; justify-content: center; align-items: flex-start; padding: 12vh 16px 16px; background: color-mix(in srgb, var(--bg-canvas) 70%, transparent); }
  .barca-palette { width: min(600px, 100%); max-height: 60vh; display: flex; flex-direction: column; background: var(--bg-raised); color: var(--text-default); border: 1px solid var(--border-default); border-radius: var(--radius-md); box-shadow: var(--elev-modal); font-family: var(--font-sans); font-size: var(--text-sm); overflow: hidden; }
  .barca-palette input { padding: 12px 14px; border: 0; border-bottom: 1px solid var(--border-default); background: transparent; color: var(--text-strong); font: inherit; outline: none; }
  .barca-palette ul { margin: 0; padding: 4px; list-style: none; overflow-y: auto; }
  .barca-palette li { display: flex; align-items: baseline; gap: 10px; padding: 6px 10px; border-radius: var(--radius-sm); cursor: pointer; }
  .barca-palette li[aria-selected="true"] { background: var(--bg-hover, var(--bg-canvas)); color: var(--text-strong); }
  .barca-palette li > b { font-weight: var(--weight-regular); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .barca-palette li > span { margin-left: auto; flex: none; color: var(--text-muted); font-size: var(--text-xs); }
  .barca-palette-empty { padding: 14px; color: var(--text-muted); }
`

export interface CommandPaletteViewProps {
  query: string
  items: readonly PaletteItem[]
  active: number
  onQuery: (value: string) => void
  onMove: (by: number) => void
  onPick: (item: PaletteItem) => void
  onClose: () => void
}

/** barca · CommandPalette: one field that reaches any node, group, file or run. */
export function CommandPaletteView({ query, items, active, onQuery, onMove, onPick, onClose }: CommandPaletteViewProps) {
  injectStyles(STYLE_ID, CSS)
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
          aria-label="Jump to a node, group, file or run"
          placeholder="Jump to a node, group, file or run"
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
        {items.length === 0 ? <p className="barca-palette-empty">Nothing matches.</p> : (
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
