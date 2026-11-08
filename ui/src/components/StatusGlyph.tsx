import type { CSSProperties, ReactNode } from 'react'
import { glyphColor, type Glyph } from '@/lib/glyph'

export interface StatusGlyphProps {
  glyph: Glyph
  /** Pixel size; 14 beside a word, 12 in dense places. */
  size?: number
  /** Live work: the running arc turns. */
  live?: boolean
  /** What it stands for, for a screen reader; without one it is decoration (a word sits beside it). */
  label?: string
  style?: CSSProperties
}

const RING = <circle cx="8" cy="8" r="6.25" stroke="currentColor" strokeWidth="1.5" />

/** The marks of each glyph, on a 16-unit grid; `currentColor` is the state's hue. */
function marks(glyph: Glyph): ReactNode {
  switch (glyph) {
    case 'fresh':
      return (
        <>
          <circle cx="8" cy="8" r="7" fill="currentColor" />
          <path d="M5.1 8.25 7.1 10.2 10.9 6.1" stroke="var(--glyph-ink)" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" />
        </>
      )
    case 'failed':
      return (
        <>
          <circle cx="8" cy="8" r="7" fill="currentColor" />
          <path d="M5.75 5.75 10.25 10.25M10.25 5.75 5.75 10.25" stroke="var(--glyph-ink)" strokeWidth="1.6" strokeLinecap="round" />
        </>
      )
    case 'running':
      return (
        <>
          <circle cx="8" cy="8" r="6.25" stroke="currentColor" strokeWidth="1.5" opacity="0.3" />
          <g className="barca-glyph-spin">
            <path d="M8 1.75A6.25 6.25 0 0 1 14.25 8" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" />
          </g>
          <circle cx="8" cy="8" r="2" fill="currentColor" />
        </>
      )
    case 'queued':
      return (
        <>
          {RING}
          <path d="M8 4.9V8l2 1.3" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round" />
        </>
      )
    case 'stale':
      return (
        <>
          {RING}
          <path d="M8 4a4 4 0 0 1 0 8z" fill="currentColor" />
        </>
      )
    case 'partial':
      return (
        <>
          {RING}
          <path d="M8 8V4a4 4 0 1 1-4 4z" fill="currentColor" />
        </>
      )
    case 'blocked':
      return (
        <>
          {RING}
          <path d="M3.6 3.6 12.4 12.4" stroke="currentColor" strokeWidth="1.5" />
        </>
      )
    case 'unsynced':
      return (
        <>
          {RING}
          <path d="M8 11V5.3M5.6 7.6 8 5.2l2.4 2.4" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round" />
        </>
      )
    case 'unknown':
      return (
        <>
          {RING}
          <path d="M6.3 6.4a1.75 1.75 0 1 1 2.5 1.6c-.5.25-.8.6-.8 1.15" stroke="currentColor" strokeWidth="1.35" strokeLinecap="round" />
          <circle cx="8" cy="11.1" r="0.8" fill="currentColor" />
        </>
      )
    case 'never_run':
      return <circle cx="8" cy="8" r="6.25" stroke="currentColor" strokeWidth="1.5" strokeDasharray="2.45 2.46" />
    case 'always':
      return (
        <>
          <path d="M13.75 8A5.75 5.75 0 1 1 11.9 3.75" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
          <path d="M12.4 1.4v2.8H9.6" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" />
        </>
      )
    case 'pinned':
      return (
        <>
          {RING}
          <circle cx="8" cy="8" r="2.75" fill="currentColor" />
        </>
      )
    case 'paused':
      return (
        <>
          {RING}
          <path d="M6.5 5.5v5M9.5 5.5v5" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
        </>
      )
    case 'cancelled':
      return (
        <>
          {RING}
          <rect x="5.75" y="5.75" width="4.5" height="4.5" rx="0.75" fill="currentColor" />
        </>
      )
    case 'skipped':
      return (
        <>
          {RING}
          <path d="M5.25 8h5.5" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
        </>
      )
    case 'cached':
      return (
        <>
          {RING}
          <path d="M5.4 8.2 7.25 10 10.6 6.3" stroke="currentColor" strokeWidth="1.45" strokeLinecap="round" strokeLinejoin="round" />
        </>
      )
    case 'pending':
      return <circle cx="8" cy="8" r="6.25" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeDasharray="0.01 3.27" opacity="0.8" />
  }
}

/**
 * barca · StatusGlyph
 * The one way a state is drawn: a small shape in the state's hue (`lib/glyph.ts`). The shape
 * alone tells states apart (a filled check, a cross, a half-filled ring, a dashed ring), so
 * colour is never the only carrier, and nothing around it is coloured. Give it a `label`, or put
 * the word beside it (`StatusBadge`).
 */
export function StatusGlyph({ glyph, size = 14, live = false, label, style }: StatusGlyphProps) {
  return (
    <svg
      className="barca-glyph"
      data-glyph={glyph}
      data-live={(live && glyph === 'running') || undefined}
      width={size}
      height={size}
      viewBox="0 0 16 16"
      fill="none"
      role={label ? 'img' : undefined}
      aria-label={label}
      aria-hidden={label ? undefined : true}
      style={{ color: glyphColor(glyph), ...style }}
    >
      {marks(glyph)}
    </svg>
  )
}
