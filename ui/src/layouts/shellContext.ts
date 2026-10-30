import type { RunButton } from '@/lib/runTarget'

/** What a page offers the topbar's Run button; `null` hides the button. */
export interface TopbarRun {
  onRun: () => void
  /** The button's words: what it runs and how much. */
  label: string
  disabled: boolean
  loading: boolean
  title?: string
  /** Asked before `onRun`; null runs at once. */
  confirm: RunButton['confirm']
}

/** Outlet context from `AppShell`: pages register their Run action here. */
export interface AppShellContext {
  setTopbarRun: (run: TopbarRun | null) => void
}
