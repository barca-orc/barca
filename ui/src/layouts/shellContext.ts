/** What a page offers the topbar's Run button; `null` hides the button. */
export interface TopbarRun {
  onRun: () => void
  disabled: boolean
  loading: boolean
  title?: string
}

/** Outlet context from `AppShell`: pages register their Run action here. */
export interface AppShellContext {
  setTopbarRun: (run: TopbarRun | null) => void
}
