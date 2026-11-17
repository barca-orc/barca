import { mapPartColor, type MapPart } from '@/lib/causeDisplay'

export interface CauseDotProps {
  /** A cause as the server sends it, or `fresh`. */
  part: MapPart
  size?: number
}

/**
 * barca · CauseDot
 * The colour the health map gives a cause (or fresh), as a dot: what a graph card, a group's
 * parts, a line of the sidebar and the legend draw, so a colour means one thing on every page.
 * The word sits beside it, or in the tooltip and accessible name of what it marks.
 */
export function CauseDot({ part, size = 9 }: CauseDotProps) {
  return <i className="barca-cause-dot" data-cause={part} style={{ background: mapPartColor(part), width: size, height: size }} aria-hidden />
}
