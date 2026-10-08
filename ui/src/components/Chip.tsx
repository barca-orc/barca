import type { ReactNode } from 'react'

export interface ChipProps {
  /** Whether the filter is on. */
  pressed: boolean
  onClick: () => void
  children: ReactNode
}

/** barca · Chip — a toggle filter. Render inside a `ChipGroup`. */
export function Chip({ pressed, onClick, children }: ChipProps) {
  return (
    <button
      type="button"
      className={'barca-chip' + (pressed ? ' is-on' : '')}
      aria-pressed={pressed}
      onClick={onClick}
    >
      {children}
    </button>
  )
}

export interface ChipGroupProps {
  /** What the chips filter by; read out by screen readers. */
  label: string
  children: ReactNode
}

/** barca · ChipGroup — a labelled row of `Chip`s that wraps. */
export function ChipGroup({ label, children }: ChipGroupProps) {
  return (
    <div className="barca-chips" role="group" aria-label={label}>
      {children}
    </div>
  )
}
