import { useEffect, useState } from 'react'
import { Outlet } from 'react-router'
import { Sidebar } from './Sidebar'
import { LoadErrors } from './LoadErrors'
import { CommandPalette } from './CommandPalette'
import { ConfirmDialog } from '@/components'
import { OfflineBanner } from '@/components/OfflineBanner'
import { PausedBanner } from '@/components/PausedBanner'
import { PinnedBanner } from '@/components/PinnedBanner'
import type { AppShellContext, TopbarRun } from './shellContext'

/**
 * The frame: the sidebar (the one navigation, search and settings), and the open page beside
 * it, under any banner. No top bar: each page's toolbar is its first line.
 */
export function AppShell() {
  const [topbarRun, setTopbarRun] = useState<TopbarRun | null>(null)
  const [palette, setPalette] = useState(false)
  const [confirming, setConfirming] = useState(false)
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'k') {
        e.preventDefault()
        setPalette((open) => !open)
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [])
  const run = topbarRun
  const startRun = () => (run?.confirm ? setConfirming(true) : run?.onRun())

  return (
    <div className="barca-app">
      <Sidebar onSearch={() => setPalette(true)} />
      <main className="barca-main">
        <OfflineBanner />
        <PausedBanner />
        <PinnedBanner />
        <LoadErrors />
        <div className="barca-content">
          <Outlet context={{ setTopbarRun } satisfies AppShellContext} />
        </div>
      </main>
      {palette && <CommandPalette run={run} onRun={startRun} onClose={() => setPalette(false)} />}
      {run?.confirm && confirming && (
        <ConfirmDialog
          {...run.confirm}
          onCancel={() => setConfirming(false)}
          onConfirm={() => {
            setConfirming(false)
            run.onRun()
          }}
        />
      )}
    </div>
  )
}
