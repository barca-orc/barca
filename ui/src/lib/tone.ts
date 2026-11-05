/**
 * Colour is status: the design system's hues as a closed set, and the CSS tokens of each. A
 * tone says how something looks, never what it is: the word beside it says that. Two axes map
 * onto tones and nowhere else: a node's cache state (`cacheStateDisplay.ts`) and a run's
 * lifecycle (`status.ts`).
 */

export type Tone = 'success' | 'warning' | 'failed' | 'running' | 'queued' | 'neutral'

export const TONES: readonly Tone[] = ['success', 'warning', 'failed', 'running', 'queued', 'neutral']

export interface ToneMeta {
  /** CSS var for the colour. */
  color: string
  /** CSS var for the faint tinted background. */
  bg: string
  /** CSS var for the border line. */
  line: string
}

const META = {
  success: { color: 'var(--status-success)', bg: 'var(--status-success-bg)', line: 'var(--status-success-line)' },
  warning: { color: 'var(--status-warning)', bg: 'var(--status-warning-bg)', line: 'var(--status-warning-line)' },
  failed: { color: 'var(--status-failed)', bg: 'var(--status-failed-bg)', line: 'var(--status-failed-line)' },
  running: { color: 'var(--status-running)', bg: 'var(--status-running-bg)', line: 'var(--status-running-line)' },
  queued: { color: 'var(--status-queued)', bg: 'var(--status-queued-bg)', line: 'var(--status-queued-line)' },
  neutral: { color: 'var(--status-skipped)', bg: 'var(--status-skipped-bg)', line: 'transparent' },
} satisfies Record<Tone, ToneMeta>

/** The design tokens of a tone. */
export function toneMeta(tone: Tone): ToneMeta {
  return META[tone]
}
