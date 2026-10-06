import type { ButtonHTMLAttributes, CSSProperties, ReactNode } from 'react'
import { injectStyles } from '@/lib/injectStyles'

export type ButtonVariant = 'signal' | 'neutral' | 'ghost' | 'danger'
export type ButtonSize = 'sm' | 'md' | 'lg'

export interface ButtonProps
  extends Omit<ButtonHTMLAttributes<HTMLButtonElement>, 'style'> {
  variant?: ButtonVariant
  size?: ButtonSize
  loading?: boolean
  fullWidth?: boolean
  iconLeft?: ReactNode
  iconRight?: ReactNode
  style?: CSSProperties
}

const SIZES = {
  sm: { height: 26, padding: '0 10px', fontSize: 'var(--text-sm)', gap: 6, radius: 'var(--radius-sm)' },
  md: { height: 32, padding: '0 14px', fontSize: 'var(--text-base)', gap: 7, radius: 'var(--radius-sm)' },
  lg: { height: 40, padding: '0 20px', fontSize: 'var(--text-md)', gap: 8, radius: 'var(--radius-md)' },
} as const

const VARIANTS: Record<ButtonVariant, CSSProperties> = {
  signal: { background: 'var(--signal)', color: 'var(--text-on-signal)', border: '1px solid transparent', fontWeight: 600 },
  neutral: { background: 'var(--bg-raised)', color: 'var(--text-strong)', border: '1px solid var(--border-strong)', fontWeight: 500 },
  ghost: { background: 'transparent', color: 'var(--text-default)', border: '1px solid transparent', fontWeight: 500 },
  danger: { background: 'transparent', color: 'var(--status-failed)', border: '1px solid var(--status-failed-line)', fontWeight: 500 },
}

const STYLE_ID = 'barca-button-styles'
const CSS = `
  @keyframes barca-spin { to { transform: rotate(360deg); } }
  .barca-btn--signal:hover:not(:disabled) { background: var(--signal-hover) !important; }
  .barca-btn--signal:active:not(:disabled) { background: var(--signal-press) !important; transform: translateY(0.5px); }
  .barca-btn--neutral:hover:not(:disabled) { border-color: var(--c-ink-300) !important; background: var(--bg-overlay) !important; }
  .barca-btn--neutral:active:not(:disabled) { transform: translateY(0.5px); }
  .barca-btn--ghost:hover:not(:disabled) { background: var(--bg-hover) !important; }
  .barca-btn--ghost:active:not(:disabled) { background: var(--bg-active) !important; }
  .barca-btn--danger:hover:not(:disabled) { background: var(--status-failed-bg) !important; border-color: var(--status-failed) !important; }
  .barca-btn:focus-visible { box-shadow: var(--focus-glow) !important; }
`

/**
 * barca · Button
 * The primary action control. The "signal" variant uses the cool emerald
 * brand green and is reserved for the single most important action in a
 * view. Everything else is quiet.
 */
export function Button({
  variant = 'neutral',
  size = 'md',
  disabled = false,
  loading = false,
  fullWidth = false,
  iconLeft,
  iconRight,
  style,
  children,
  ...rest
}: ButtonProps) {
  injectStyles(STYLE_ID, CSS)
  const s = SIZES[size]
  return (
    <button
      className={`barca-btn barca-btn--${variant}`}
      disabled={disabled || loading}
      data-loading={loading ? '' : undefined}
      style={{
        display: 'inline-flex',
        alignItems: 'center',
        justifyContent: 'center',
        gap: s.gap,
        height: s.height,
        padding: s.padding,
        width: fullWidth ? '100%' : undefined,
        fontFamily: 'var(--font-sans)',
        fontSize: s.fontSize,
        lineHeight: 1,
        letterSpacing: '-0.005em',
        borderRadius: s.radius,
        cursor: disabled || loading ? 'not-allowed' : 'pointer',
        opacity: disabled ? 0.45 : 1,
        whiteSpace: 'nowrap',
        userSelect: 'none',
        transition: 'var(--t-all)',
        outline: 'none',
        ...VARIANTS[variant],
        ...style,
      }}
      {...rest}
    >
      {loading ? <Spinner /> : iconLeft}
      {children}
      {!loading && iconRight}
    </button>
  )
}

function Spinner() {
  return (
    <span
      aria-hidden
      style={{
        width: '1em',
        height: '1em',
        borderRadius: '50%',
        border: '1.6px solid currentColor',
        borderTopColor: 'transparent',
        display: 'inline-block',
        animation: 'barca-spin 0.7s linear infinite',
        opacity: 0.9,
      }}
    />
  )
}
