/**
 * Where the barca HTTP API lives, relative to the page.
 *
 * `barca serve` serves the UI at `<prefix>/ui/` and the API at `<prefix>/`,
 * where `<prefix>` is empty when you open the server directly and is whatever
 * path a reverse proxy mounts it under otherwise (e.g. nginx at `/barca/`).
 * The UI routes with the URL hash, so the page path is always the UI root and
 * everything before its `ui` segment is the API base — no configuration.
 *
 * In dev, Vite serves the UI at `/ui/` too and proxies everything else to
 * `barca serve`, so the same rule holds.
 */

/** The API base for a page at `pathname`: `''` at the server root, else the prefix. */
export function apiBase(pathname: string): string {
  const path = pathname.replace(/\/index\.html$/, '/')
  const m = /^(.*)\/ui\/?$/.exec(path)
  if (m) return m[1] ?? ''
  // Not under a `ui` segment (shouldn't happen when served by barca): treat the
  // page's directory as the base.
  return path.replace(/\/$/, '')
}

/** An absolute-path URL for an API route (`path` starts with `/`). */
export function apiUrl(base: string, path: string): string {
  return `${base}${path}`
}

/** The API base for the running page. */
export const API_BASE = typeof window === 'undefined' ? '' : apiBase(window.location.pathname)
