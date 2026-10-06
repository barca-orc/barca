import type { CSSProperties, ReactNode } from 'react'

export type TagTone = 'default' | 'signal' | 'bare'
export type TagSize = 'sm' | 'md'

export interface TagProps {
  tone?: TagTone
  size?: TagSize
  /** Show a leading dot (signal tone). */
  dot?: boolean
  style?: CSSProperties
  children: ReactNode
}

const SIZES = {
  sm: { height: 18, padding: '0 6px', fontSize: 'var(--text-2xs)' },
  md: { height: 20, padding: '0 8px', fontSize: 'var(--text-xs)' },
} as const

/**
 * barca · Tag
 * A small mono label for metadata (owners, kinds, partitions).
 */
export function Tag({ tone = 'default', size = 'sm', dot = false, style, children }: TagProps) {
  const s = SIZES[size]
  const toneStyle: CSSProperties =
    tone === 'signal'
      ? { background: 'var(--c-green-wash)', border: '1px solid var(--c-green-line)', color: 'var(--signal)' }
      : tone === 'bare'
        ? { background: 'transparent', border: '1px solid var(--border-default)', color: 'var(--text-muted)' }
        : { background: 'var(--bg-inset)', border: '1px solid var(--border-default)', color: 'var(--text-default)' }
  return (
    <span
      style={{
        display: 'inline-flex',
        alignItems: 'center',
        gap: 5,
        height: s.height,
        padding: s.padding,
        borderRadius: 'var(--radius-sm)',
        fontFamily: 'var(--font-mono)',
        fontSize: s.fontSize,
        fontWeight: 500,
        whiteSpace: 'nowrap',
        ...toneStyle,
        ...style,
      }}
    >
      {dot && (
        <span
          style={{
            width: 5,
            height: 5,
            borderRadius: '50%',
            background: 'var(--signal)',
            flex: 'none',
          }}
        />
      )}
      {children}
    </span>
  )
}
