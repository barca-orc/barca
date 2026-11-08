import type { ReactNode } from 'react'

export interface SelectOption<V extends string> {
  value: V
  label: string
}

export interface SelectProps<V extends string> {
  label: string
  value: V
  options: readonly SelectOption<V>[]
  onChange: (value: V) => void
  /** Shown in the box in place of the label (a toolbar's select); the label stays for a screen reader and the tooltip. */
  icon?: ReactNode
}

/** barca · Select — a native select with its label beside it, or an icon in its box. */
export function Select<V extends string>({ label, value, options, onChange, icon }: SelectProps<V>) {
  return (
    <label className="barca-select" title={icon ? label : undefined}>
      {icon}
      <span className={icon ? 'barca-sr-only' : undefined}>{label}</span>
      <select value={value} onChange={(e) => onChange(e.target.value as V)}>
        {options.map((o) => (
          <option key={o.value} value={o.value}>
            {o.label}
          </option>
        ))}
      </select>
    </label>
  )
}
