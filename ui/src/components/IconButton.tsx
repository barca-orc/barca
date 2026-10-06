import type { ButtonHTMLAttributes, CSSProperties, ReactNode } from 'react'
import { injectStyles } from '@/lib/injectStyles'

export type IconButtonSize = 'sm' | 'md' | 'lg'
export type IconButtonVariant = 'ghost' | 'outline'

export interface IconButtonProps
  extends Omit<ButtonHTMLAttributes<HTMLButtonElement>, 'style'> {
  /** Accessible label — required for icon-only controls. */
  label: string
  size?: IconButtonSize
  variant?: IconButtonVariant
  active?: boolean
  style?: CSSProperties
  children: ReactNode
}

const DIMS = { sm: 24, md: 30, lg: 36 } as const

const STYLE_ID = 'barca-iconbutton-styles'
const CSS = `
  .barca-iconbtn--ghost:hover:not(:disabled) { background: var(--bg-hover) !important; color: var(--text-strong) !important; }
  .barca-iconbtn--outline:hover:not(:disabled) { border-color: var(--c-ink-300) !important; color: var(--text-strong) !important; }
  .barca-iconbtn:active:not(:disabled) { transform: translateY(0.5px); }
  .barca-iconbtn:focus-visible { box-shadow: var(--focus-glow) !important; }
`

/**
 * barca · IconButton
 * A square, quiet control for icon-only actions (toolbar, row actions).
 * Pass a single icon node as children.
 */
export function IconButton({
  label,
  size = 'md',
  variant = 'ghost',
  disabled = false,
  active = false,
  style,
  children,
  ...rest
}: IconButtonProps) {
  injectStyles(STYLE_ID, CSS)
  const d = DIMS[size]
  const variantStyle: CSSProperties =
    variant === 'outline'
      ? {
          background: 'var(--bg-surface)',
          color: 'var(--text-default)',
          border: '1px solid var(--border-strong)',
        }
      : {
          background: active ? 'var(--bg-active)' : 'transparent',
          color: active ? 'var(--signal)' : 'var(--text-muted)',
          border: '1px solid transparent',
        }
  return (
    <button
      aria-label={label}
      title={label}
      disabled={disabled}
      className={`barca-iconbtn barca-iconbtn--${variant}`}
      style={{
        width: d,
        height: d,
        display: 'inline-flex',
        alignItems: 'center',
        justifyContent: 'center',
        borderRadius: 'var(--radius-sm)',
        cursor: disabled ? 'not-allowed' : 'pointer',
        opacity: disabled ? 0.4 : 1,
        transition: 'var(--t-all)',
        outline: 'none',
        flex: 'none',
        ...variantStyle,
        ...style,
      }}
      {...rest}
    >
      {children}
    </button>
  )
}
