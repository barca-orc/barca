/**
 * A name cut at the start when it does not fit: the end is what tells rows apart. The name is
 * one text node (the cut is CSS, `.barca-mid` in shell.css), so selecting the row copies it
 * whole and a screen reader gets it as one word; the full id is in the tooltip.
 */
export function MiddleName({ name, title }: { name: string; title?: string }) {
  return <span className="barca-mid" title={title ?? name}>{name}</span>
}
