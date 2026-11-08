import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react'
import {
  cellAt, errorOf, gridGeometry, keyOf, KEY_STATE_WORD, stateOf,
  type Grid, type GridLayout, type KeyRange, type KeyState,
} from '@/lib/partitionGrid'

export interface KeyGridProps {
  grid: Grid
  layout: GridLayout
  /** The cells the filters keep; null: every key. The rest are drawn faint. */
  shown: Uint8Array | null
  selected: number | null
  range: KeyRange | null
  /** A cell clicked: `extend` with Shift, to select the keys between it and the open one. */
  onPick: (at: { block: number; index: number }, extend: boolean) => void
}

const TOKENS: Record<KeyState, string> = {
  fresh: '--state-fresh',
  failed: '--state-failed',
  stale: '--state-stale',
  blocked: '--state-blocked',
  never_run: '--state-idle',
}

/**
 * barca · KeyGrid
 * Every key of a partitioned node as one cell, coloured by its state, on a canvas: 10,000 keys
 * draw in a few milliseconds and stay one picture. One block per combination of the shorter
 * dimensions, each running through the longest in order, so a failure that is one region's
 * bad week reads as a red patch in that region's block.
 */
export function KeyGrid({ grid, layout, shown, selected, range, onPick }: KeyGridProps) {
  const box = useRef<HTMLDivElement>(null)
  const canvas = useRef<HTMLCanvasElement>(null)
  const [width, setWidth] = useState(0)
  const [hover, setHover] = useState<{ x: number; y: number; cell: number } | null>(null)
  const along = grid.dims[layout.along]
  // Each row starts with its first value of the long dimension: a date, an account.
  const gutter = useMemo(() => {
    const longest = Math.max(0, ...(along?.values ?? []).map((v) => v.length))
    return longest === 0 ? 0 : Math.min(120, Math.ceil(longest * 6.2) + 12)
  }, [along])
  const geo = useMemo(() => gridGeometry(layout, Math.max(width, 160), gutter), [layout, width, gutter])

  useLayoutEffect(() => {
    const el = box.current
    if (!el) return
    const observer = new ResizeObserver(([entry]) => setWidth(Math.floor(entry!.contentRect.width)))
    observer.observe(el)
    setWidth(Math.floor(el.getBoundingClientRect().width))
    return () => observer.disconnect()
  }, [])

  useEffect(() => {
    const el = canvas.current
    if (!el || width === 0) return
    const ratio = window.devicePixelRatio || 1
    el.width = Math.ceil(width * ratio)
    el.height = Math.ceil(geo.height * ratio)
    const ctx = el.getContext('2d')
    if (!ctx) return
    ctx.setTransform(ratio, 0, 0, ratio, 0, 0)
    ctx.clearRect(0, 0, width, geo.height)
    const css = getComputedStyle(el)
    const color = (token: string) => css.getPropertyValue(token).trim() || 'gray'
    const fills = Object.fromEntries(Object.entries(TOKENS).map(([s, t]) => [s, color(t)])) as Record<KeyState, string>
    const text = color('--text-default')
    const accent = color('--accent')
    const step = geo.cell + geo.gap
    const inRange = new Set<number>()
    if (range) {
      const block = layout.blocks[range.block]
      const [lo, hi] = range.from <= range.to ? [range.from, range.to] : [range.to, range.from]
      for (let i = lo; i <= hi; i++) inRange.add(block?.cells[i] ?? -1)
    }
    const faint = color('--text-faint')
    const rowEvery = Math.max(1, Math.ceil(11 / step))
    ctx.textBaseline = 'middle'
    layout.blocks.forEach((block, b) => {
      const top = geo.tops[b]!
      ctx.globalAlpha = 1
      if (geo.labelHeight > 0) {
        ctx.font = '500 12px Inter, system-ui, sans-serif'
        ctx.fillStyle = text
        ctx.fillText(block.label.replace(/[^=, ]+=/g, ''), 0, top - geo.labelHeight / 2 - 1)
      }
      if (along && geo.gutter > 0) {
        ctx.font = '10px "JetBrains Mono", ui-monospace, monospace'
        ctx.fillStyle = faint
        const rows = Math.ceil(block.cells.length / geo.columns)
        for (let r = 0; r < rows; r += rowEvery) {
          const value = along.values[r * geo.columns]
          if (value !== undefined) ctx.fillText(value, 0, top + r * step + geo.cell / 2 + 0.5)
        }
      }
      block.cells.forEach((cell, i) => {
        const state = stateOf(grid, cell)
        if (!state) return
        const x = geo.gutter + (i % geo.columns) * step
        const y = top + Math.floor(i / geo.columns) * step
        ctx.globalAlpha = shown && !shown[cell] ? 0.16 : 1
        ctx.fillStyle = fills[state]
        ctx.fillRect(x, y, geo.cell, geo.cell)
        if (inRange.has(cell)) {
          ctx.globalAlpha = 1
          ctx.strokeStyle = accent
          ctx.lineWidth = 1
          ctx.strokeRect(x + 0.5, y + 0.5, geo.cell - 1, geo.cell - 1)
        }
      })
    })
    ctx.globalAlpha = 1
    if (selected !== null) {
      layout.blocks.forEach((block, b) => {
        const i = block.cells.indexOf(selected)
        if (i < 0) return
        const x = geo.gutter + (i % geo.columns) * step
        const y = geo.tops[b]! + Math.floor(i / geo.columns) * step
        ctx.strokeStyle = text
        ctx.lineWidth = 2
        ctx.strokeRect(x - 1, y - 1, geo.cell + 2, geo.cell + 2)
      })
    }
  }, [grid, layout, geo, width, shown, selected, range, along])

  const at = (e: React.MouseEvent) => {
    const rect = canvas.current!.getBoundingClientRect()
    const x = e.clientX - rect.left
    const y = e.clientY - rect.top
    return { x, y, hit: cellAt(layout, geo, x, y) }
  }

  const hovered = hover === null ? null : {
    key: keyOf(grid, hover.cell),
    state: stateOf(grid, hover.cell),
    error: errorOf(grid, hover.cell)?.text ?? null,
  }

  return (
    <div className="barca-keygrid" ref={box}>
      {along && geo.labelHeight > 0 && (
        <p className="barca-keygrid-axis">
          Each row runs through <b>{along.name}</b>, {along.values[0]} → {along.values[along.values.length - 1]}
        </p>
      )}
      <canvas
        ref={canvas}
        role="img"
        aria-label={`Every key, coloured by state: ${layout.blocks.length} ${layout.blocks.length === 1 ? 'block' : 'blocks'} of ${along?.name ?? 'keys'}`}
        style={{ width, height: geo.height, cursor: hover ? 'pointer' : 'default' }}
        onMouseMove={(e) => {
          const { x, y, hit } = at(e)
          const cell = hit ? layout.blocks[hit.block]!.cells[hit.index]! : null
          setHover(cell !== null && stateOf(grid, cell) ? { x, y, cell } : null)
        }}
        onMouseLeave={() => setHover(null)}
        onClick={(e) => {
          const { hit } = at(e)
          if (hit && stateOf(grid, layout.blocks[hit.block]!.cells[hit.index]!)) onPick(hit, e.shiftKey)
        }}
      />
      {hover && hovered && (
        <div
          className="barca-keygrid-tip"
          role="tooltip"
          style={{ left: Math.min(hover.x + 12, Math.max(0, width - 280)), top: hover.y + 14 }}
        >
          <code>{hovered.key}</code>
          <span>{hovered.state ? KEY_STATE_WORD[hovered.state] : ''}</span>
          {hovered.error && <span className="barca-keygrid-tip-error">{hovered.error}</span>}
        </div>
      )}
    </div>
  )
}
