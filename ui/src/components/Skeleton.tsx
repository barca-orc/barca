import type { CSSProperties } from 'react'

export interface SkeletonProps {
  /** CSS width: a number is px. Default fills the container. */
  width?: number | string
  /** CSS height: a number is px. Give it the height of what will replace it. */
  height?: number | string
  style?: CSSProperties
}

/**
 * barca · Skeleton
 * A placeholder that holds the space of content still loading. Size it to what will
 * replace it: the point is that nothing moves when the data lands, not the shimmer.
 */
export function Skeleton({ width = '100%', height = 12, style }: SkeletonProps) {
  return <span className="barca-skeleton" aria-hidden="true" style={{ width, height, ...style }} />
}
