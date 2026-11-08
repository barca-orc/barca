import type { ReactNode } from 'react'

/**
 * barca · EmptyNote
 * What a page says when it has nothing to list at all: a short title and one helpful sentence,
 * centred in the page's body. A filter that matches nothing says so in a line instead.
 */
export function EmptyNote({ title, children }: { title: string; children: ReactNode }) {
  return (
    <div className="barca-empty-note" role="note">
      <strong>{title}</strong>
      <p>{children}</p>
    </div>
  )
}
