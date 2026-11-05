import type { ButtonHTMLAttributes, CSSProperties, ReactNode } from 'react'

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

/**
 * barca · Button
 * The primary action control. The "signal" variant is filled and neutral
 * (green means healthy, never "press me") and is reserved for the single most important action in a
 * view. Everything else is quiet. Styled by `.barca-btn` in styles/components.css.
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
  className,
  children,
  ...rest
}: ButtonProps) {
  return (
    <button
      className={['barca-btn', `barca-btn--${variant}`, className].filter(Boolean).join(' ')}
      data-size={size}
      data-full={fullWidth || undefined}
      disabled={disabled || loading}
      data-loading={loading ? '' : undefined}
      style={style}
      {...rest}
    >
      {loading ? <span className="barca-spinner" aria-hidden /> : iconLeft}
      {children}
      {!loading && iconRight}
    </button>
  )
}
