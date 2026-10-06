/**
 * Inject a <style> block into <head> exactly once, keyed by id.
 * Guarded by document.getElementById so it survives re-renders and
 * bundler scope quirks (the pattern the design system converged on).
 */
export function injectStyles(id: string, css: string): void {
  if (typeof document === 'undefined') return
  if (document.getElementById(id)) return
  const el = document.createElement('style')
  el.id = id
  el.textContent = css
  document.head.appendChild(el)
}
