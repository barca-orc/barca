import type { CSSProperties, ReactNode } from 'react'

export type TagTone = 'default' | 'signal' | 'bare' | 'failed' | 'warning'
export type TagSize = 'sm' | 'md'

export interface TagProps {
  tone?: TagTone
  size?: TagSize
  /** Show a leading dot (signal tone). */
  dot?: boolean
  style?: CSSProperties
  children: ReactNode
}

/**
 * barca · Tag
 * A small label for metadata (kinds, partitions, read-only) and for the fact beside a state
 * (`failed`: the last attempt failed).
 */
export function Tag({ tone = 'default', size = 'sm', dot = false, style, children }: TagProps) {
  return (
    <span className="barca-tag" data-tone={tone} data-size={size} style={style}>
      {dot && <span className="barca-tag-dot" />}
      {children}
    </span>
  )
}
