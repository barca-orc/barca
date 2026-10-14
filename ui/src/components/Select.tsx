export interface SelectOption<V extends string> {
  value: V
  label: string
}

export interface SelectProps<V extends string> {
  label: string
  value: V
  options: readonly SelectOption<V>[]
  onChange: (value: V) => void
}

/** barca · Select — a native select with its label beside it. */
export function Select<V extends string>({ label, value, options, onChange }: SelectProps<V>) {
  return (
    <label className="barca-select">
      <span>{label}</span>
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
