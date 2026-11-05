import type { ButtonHTMLAttributes, CSSProperties, ReactNode } from 'react'

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

/**
 * barca · IconButton
 * A square, quiet control for icon-only actions (toolbar, row actions).
 * Pass a single icon node as children. Styled by `.barca-iconbtn` in styles/components.css.
 */
export function IconButton({
  label,
  size = 'md',
  variant = 'ghost',
  disabled = false,
  active = false,
  style,
  className,
  children,
  ...rest
}: IconButtonProps) {
  return (
    <button
      aria-label={label}
      title={label}
      disabled={disabled}
      className={['barca-iconbtn', `barca-iconbtn--${variant}`, className].filter(Boolean).join(' ')}
      data-size={size}
      data-active={active || undefined}
      style={style}
      {...rest}
    >
      {children}
    </button>
  )
}
