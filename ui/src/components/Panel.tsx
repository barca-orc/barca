import type { CSSProperties, ReactNode } from 'react'

export interface PanelProps {
  title?: ReactNode
  eyebrow?: ReactNode
  actions?: ReactNode
  /** Apply default body padding (ignored when `flush`). */
  padded?: boolean
  /** Remove all body padding. */
  flush?: boolean
  style?: CSSProperties
  children?: ReactNode
}

/**
 * barca · Panel
 * The base surface container — optional header (eyebrow + title + actions)
 * over a body. Most console regions are panels.
 */
export function Panel({
  title,
  eyebrow,
  actions,
  padded = true,
  flush = false,
  style,
  children,
}: PanelProps) {
  const hasHeader = title || actions || eyebrow
  return (
    <section
      style={{
        background: 'var(--bg-surface)',
        border: '1px solid var(--border-default)',
        borderRadius: 'var(--radius-lg)',
        boxShadow: 'var(--shadow-sm), var(--edge-top)',
        overflow: 'hidden',
        ...style,
      }}
    >
      {hasHeader && (
        <header
          style={{
            display: 'flex',
            alignItems: 'center',
            justifyContent: 'space-between',
            gap: 12,
            padding: '10px 14px',
            borderBottom: '1px solid var(--border-subtle)',
            minHeight: 44,
          }}
        >
          <div style={{ minWidth: 0 }}>
            {eyebrow && (
              <div
                style={{
                  fontFamily: 'var(--font-mono)',
                  fontSize: 'var(--text-2xs)',
                  letterSpacing: 'var(--tracking-caps)',
                  textTransform: 'uppercase',
                  color: 'var(--text-faint)',
                  marginBottom: 2,
                }}
              >
                {eyebrow}
              </div>
            )}
            {title && (
              <h3
                style={{
                  margin: 0,
                  fontFamily: 'var(--font-display)',
                  fontSize: 'var(--text-md)',
                  fontWeight: 600,
                  color: 'var(--text-strong)',
                  letterSpacing: '-0.01em',
                  whiteSpace: 'nowrap',
                  overflow: 'hidden',
                  textOverflow: 'ellipsis',
                }}
              >
                {title}
              </h3>
            )}
          </div>
          {actions && <div style={{ display: 'flex', gap: 6, flex: 'none' }}>{actions}</div>}
        </header>
      )}
      <div style={{ padding: flush ? 0 : padded ? '14px' : 0 }}>{children}</div>
    </section>
  )
}
