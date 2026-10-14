import type { Page } from '@playwright/test'

/** One browser `layout-shift` entry: how much moved, when, and which elements. */
export interface Shift {
  value: number
  at: number
  elements: string[]
}

interface LayoutShiftEntry extends PerformanceEntry {
  value: number
  hadRecentInput: boolean
  sources: { node: Node | null }[]
}

declare global {
  interface Window {
    __shifts: Shift[]
  }
}

/**
 * Record layout shifts from the first paint on. Call before `page.goto`. The browser leaves
 * out shifts within 500ms of a click or key press (the user caused those), so a test that
 * wants to catch content arriving late must make it arrive later than that (`slowApi`).
 */
export async function trackLayoutShift(page: Page) {
  await page.addInitScript(() => {
    window.__shifts = []
    new PerformanceObserver((list) => {
      for (const entry of list.getEntries() as LayoutShiftEntry[]) {
        if (entry.hadRecentInput) continue
        window.__shifts.push({
          value: entry.value,
          at: Math.round(entry.startTime),
          elements: entry.sources.map((s) => {
            const el = s.node instanceof Element ? s.node : s.node?.parentElement
            const cls = (el?.getAttribute?.('class') ?? '').split(' ').filter(Boolean).slice(0, 2)
            return el ? [el.tagName.toLowerCase(), ...cls].join('.') : '(removed)'
          }),
        })
      }
    }).observe({ type: 'layout-shift', buffered: true })
  })
}

/** The shifts since the last call (or since tracking began), and their total. */
export async function takeShifts(page: Page, settleMs = 400) {
  await page.waitForTimeout(settleMs)
  const shifts = await page.evaluate(() => window.__shifts.splice(0))
  const total = shifts.reduce((sum, s) => sum + s.value, 0)
  return { total, shifts }
}

/** A readable list of shifts, for an assertion message. */
export function describeShifts(shifts: Shift[]): string {
  return shifts.map((s) => `${s.value.toFixed(4)} @${s.at}ms ${[...new Set(s.elements)].join(' | ')}`).join('\n')
}

/**
 * Hold every API response back by `ms`, so each page spends a visible moment in its loading
 * state and the shift when the data lands is measured. Static files are not delayed.
 */
export async function slowApi(page: Page, ms = 800) {
  await page.route(/\/(state|assets|health)(\/|$|\?)/, async (route) => {
    await new Promise((resolve) => setTimeout(resolve, ms))
    await route.continue()
  })
}
