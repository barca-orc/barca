import { splitMiddle } from '@/lib/names'

/** A name cut in the middle when it does not fit: the end is what tells rows apart. */
export function MiddleName({ name, title }: { name: string; title?: string }) {
  const { head, tail } = splitMiddle(name)
  return (
    <span className="barca-mid" title={title ?? name} aria-label={name}>
      <span className="barca-mid-head">{head}</span>
      {tail && <span className="barca-mid-tail">{tail}</span>}
    </span>
  )
}
