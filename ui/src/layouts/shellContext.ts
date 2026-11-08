import type { RunButton } from '@/lib/runTarget'

/**
 * A page's Run action: what its toolbar's button runs (`PageRunButton`), and what the command
 * palette (⌘K) offers; `null` when the page has nothing to run.
 */
export interface TopbarRun {
  onRun: () => void
  /** The button's words: what it runs and how much. */
  label: string
  /** The words without the size: the toolbar button's text. */
  short: string
  disabled: boolean
  loading: boolean
  title?: string
  /** Asked before `onRun`; null runs at once. */
  confirm: RunButton['confirm']
}

/** Outlet context from `AppShell`: pages register their Run action here, for the command palette. */
export interface AppShellContext {
  setTopbarRun: (run: TopbarRun | null) => void
}
