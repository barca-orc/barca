import { mapPartColor, mapPartLabel, mapPartMeaning, mapPartOf, type Cause } from '@/lib/causeDisplay'

export interface CauseBadgeProps {
  /** The node's `cause` as the server sends it; `null` for a fresh node. */
  cause: Cause | null
}

/**
 * barca · CauseBadge
 * Why a node is not fresh, in the word and colour the health map uses for it (`catching up`,
 * `no refresh path`, …), or `fresh`: one vocabulary and one legend for a page. The cause is the
 * server's (`cause` of `GET /state`); this maps it to its word and colour.
 */
export function CauseBadge({ cause }: CauseBadgeProps) {
  const part = mapPartOf(cause)
  return (
    <span className="barca-cause" data-cause={part} title={mapPartMeaning(part)}>
      <i style={{ background: mapPartColor(part) }} aria-hidden />
      {mapPartLabel(part)}
    </span>
  )
}
