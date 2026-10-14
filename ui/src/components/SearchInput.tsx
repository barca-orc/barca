import { Search } from 'lucide-react'

export interface SearchInputProps {
  value: string
  onChange: (value: string) => void
  placeholder?: string
  autoFocus?: boolean
}

/**
 * barca · SearchInput
 * A filter box for a list or table: search icon, mono text, one focus ring on the box.
 */
export function SearchInput({ value, onChange, placeholder = 'Search', autoFocus }: SearchInputProps) {
  return (
    <label className="barca-search">
      <Search size={13} />
      <input
        placeholder={placeholder}
        aria-label={placeholder}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        autoFocus={autoFocus}
      />
    </label>
  )
}
