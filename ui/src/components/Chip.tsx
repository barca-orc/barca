import type { ReactNode } from 'react'

export interface ChipProps {
  /** Whether the filter is on. */
  pressed: boolean
  onClick: () => void
  /** More than the chip says: what the filter's rows hold up, say. */
  title?: string
  children: ReactNode
}

/**
 * barca · Chip — a toggle filter. Render inside a `ChipGroup`. Always neutral: a state chip
 * says its state with a `StatusGlyph` inside it, never with a coloured border or fill.
 */
export function Chip({ pressed, onClick, title, children }: ChipProps) {
  return (
    <button
      type="button"
      className={'barca-chip' + (pressed ? ' is-on' : '')}
      aria-pressed={pressed}
      title={title}
      onClick={onClick}
    >
      {children}
    </button>
  )
}

export interface ChipGroupProps {
  /** What the chips filter by; read out by screen readers. */
  label: string
  /** Joined into one control, like a segmented button (a page header's state filters). */
  segmented?: boolean
  children: ReactNode
}

/** barca · ChipGroup — a labelled row of `Chip`s that wraps, or one segmented control. */
export function ChipGroup({ label, segmented = false, children }: ChipGroupProps) {
  return (
    <div className="barca-chips" role="group" aria-label={label} data-segmented={segmented || undefined}>
      {children}
    </div>
  )
}
