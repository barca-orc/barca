import { Skeleton } from './Skeleton'
import type { EstimateWords } from '@/lib/estimates'

export interface EstimateCellProps {
  /** What the row says (`nodeEstimate`, `scopeEstimate`); `null` while its rollup or state loads. */
  words: EstimateWords | null
}

/**
 * barca · EstimateCell
 * When a row will be fresh, as the server estimated it: `fresh ~15:05`, `all fresh by ~15:02`,
 * `rechecked 15:00`, or why there is no estimate. Only an alarm is a step stronger; a time is
 * plain. Until the data arrives it holds its place with a skeleton one text line high.
 */
export function EstimateCell({ words }: EstimateCellProps) {
  if (!words) return <span className="barca-age-loading" role="status" aria-label="Loading estimate"><Skeleton width={72} height={10} /></span>
  return <span className="barca-estimate" data-estimate={words.kind} title={words.title || undefined}>{words.text}</span>
}
