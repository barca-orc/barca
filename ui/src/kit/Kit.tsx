import { useState, type ReactNode } from 'react'
import { Chip, ChipGroup } from '@/components'

/** A titled block of the kit page; `id` is what the contents bar scrolls to. */
export function Spec({ id, title, note, children }: { id?: string; title: string; note?: ReactNode; children: ReactNode }) {
  return (
    <section className="barca-kit-spec" id={id}>
      <h2>{title}</h2>
      {note && <p className="barca-kit-note">{note}</p>}
      <div className="barca-kit-row">{children}</div>
    </section>
  )
}

/** One state, with its name under it. */
export function Variant({ label, children }: { label: string; children: ReactNode }) {
  return (
    <figure className="barca-kit-variant">
      {children}
      <figcaption>{label}</figcaption>
    </figure>
  )
}

/**
 * A box that contains `position: fixed` content (a dialog's backdrop): a transformed
 * ancestor is the containing block of its fixed descendants.
 */
export function Frame({ height, width, children }: { height: number; width?: number | string; children: ReactNode }) {
  return <div className="barca-kit-frame" style={{ height, width }}>{children}</div>
}

/**
 * One of `states` at a time, picked with chips, for views too big to show side by side.
 * `render` gets a `dispatch` that lists the messages the view sends, newest first: what a
 * click asks the program to do, with no program running.
 */
export function States<T>({ label, states, render }: {
  label: string
  states: Record<string, T>
  render: (state: T, dispatch: (msg: unknown) => void) => ReactNode
}) {
  const names = Object.keys(states)
  const [picked, setPicked] = useState(names[0]!)
  const [sent, setSent] = useState<string[]>([])
  const state = states[picked] ?? states[names[0]!]!
  return (
    <div className="barca-kit-states">
      <ChipGroup label={label}>
        {names.map((name) => (
          <Chip key={name} pressed={name === picked} onClick={() => { setPicked(name); setSent([]) }}>
            {name}
          </Chip>
        ))}
      </ChipGroup>
      <div className="barca-kit-stage">{render(state, (msg) => setSent((prev) => [JSON.stringify(msg), ...prev].slice(0, 5)))}</div>
      <div className="barca-kit-msgs" aria-label="Messages sent">
        {sent.length === 0 ? <span>No messages yet: interact with the view above.</span> : sent.map((m, i) => <code key={i}>{m}</code>)}
      </div>
    </div>
  )
}
