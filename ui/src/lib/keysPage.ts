/**
 * The key explorer's address: which node, which key is open, which keys are shown and in what
 * order. The URL is the page's model, so a filtered view or an open key is a link someone can
 * paste; the keys themselves are asked of the server with the same filters
 * (`GET /assets/{id}/partitions/keys`, what `barca status NODE --keys` takes as flags).
 *
 * `#/keys?node=p.py:events&key=day=2026-01-01,region=apac&state=failed,blocked&v.region=apac&error=0&q=03-&sort=attempted`
 */

import { KEY_STATES, type KeyState } from './keyStateDisplay'

/** The orders the server lists keys in (`sort=`); `key` is the default. */
export const KEY_SORTS = ['key', 'state', 'attempted', 'elapsed', 'size', 'rows'] as const
export type KeySort = (typeof KEY_SORTS)[number]

/** Which keys the explorer shows: every filter that is set must hold. */
export interface KeyFilter {
  /** Empty: any state. */
  states: readonly KeyState[]
  /** Per dimension, the values kept; a dimension that is absent or empty keeps every value. */
  values: Readonly<Record<string, readonly string[]>>
  /** An index into the node's errors. */
  error: number | null
  /** A substring of the key, in any case. */
  q: string
  /** A stretch of the dimension the overview runs along (its values, from and to inclusive). */
  range: { from: string; to: string } | null
}

export const NO_FILTER: KeyFilter = { states: [], values: {}, error: null, q: '', range: null }

export interface KeysParams {
  node: string | null
  key: string | null
  filter: KeyFilter
  /** Every key in the table, not only those that need a run (`show=all`). */
  all: boolean
  sort: KeySort
}

const VALUE_PREFIX = 'v.'

export function keysParams(params: URLSearchParams): KeysParams {
  const states = (params.get('state') ?? '')
    .split(',')
    .filter((s): s is KeyState => (KEY_STATES as readonly string[]).includes(s))
  const values: Record<string, string[]> = {}
  for (const [name, value] of params) {
    if (name.startsWith(VALUE_PREFIX) && value !== '') values[name.slice(VALUE_PREFIX.length)] = value.split(',')
  }
  const error = params.get('error')
  const parsed = error === null || error === '' ? NaN : Number(error)
  const range = params.get('range')?.split('..')
  const sort = params.get('sort')
  return {
    node: params.get('node') || null,
    key: params.get('key') || null,
    filter: {
      states,
      values,
      error: Number.isInteger(parsed) && parsed >= 0 ? parsed : null,
      q: params.get('q') ?? '',
      range: range?.length === 2 && range[0] && range[1] ? { from: range[0], to: range[1] } : null,
    },
    all: params.get('show') === 'all',
    sort: (KEY_SORTS as readonly string[]).includes(sort ?? '') ? (sort as KeySort) : 'key',
  }
}

/** The query string of an explorer view (without `?`); empty parts are left out. */
export function keysSearch(p: Omit<KeysParams, 'sort'> & { sort?: KeySort }): string {
  const out = new URLSearchParams()
  if (p.node) out.set('node', p.node)
  if (p.key) out.set('key', p.key)
  if (p.filter.states.length > 0) out.set('state', p.filter.states.join(','))
  for (const [name, values] of Object.entries(p.filter.values)) {
    if (values.length > 0) out.set(`${VALUE_PREFIX}${name}`, values.join(','))
  }
  if (p.filter.error !== null) out.set('error', String(p.filter.error))
  if (p.filter.q !== '') out.set('q', p.filter.q)
  if (p.filter.range) out.set('range', `${p.filter.range.from}..${p.filter.range.to}`)
  if (p.all) out.set('show', 'all')
  if (p.sort && p.sort !== 'key') out.set('sort', p.sort)
  return out.toString()
}

/** The explorer of `node`, optionally at one key or with a filter: the link other pages use. */
export function keysHref(node: string, extra: { key?: string; filter?: Partial<KeyFilter> } = {}): string {
  return `/keys?${keysSearch({ node, key: extra.key ?? null, filter: { ...NO_FILTER, ...extra.filter }, all: false })}`
}

/** One dimension's values set (none: every value). */
export function withValues(f: KeyFilter, dim: string, values: readonly string[]): KeyFilter {
  const next = { ...f.values }
  if (values.length === 0) delete next[dim]
  else next[dim] = [...values]
  return { ...f, values: next }
}

/** A column header pressed: its order, or back to key order when it is already the order. */
export function toggleSort(now: KeySort, pressed: KeySort): KeySort {
  return now === pressed ? 'key' : pressed
}

/**
 * What the explorer asks the server for (`GET /assets/{id}/partitions/keys`), without the page:
 * the filters as the route spells them. `needsRun`: only the keys a run computes, the table's
 * default view. `along`: the dimension `filter.range` is a stretch of (the overview's `along`);
 * a range is sent only when the dimension has both its ends (`rangeValid`), so a stale link
 * shows every key, not an error.
 */
export function keysQuery(p: { filter: KeyFilter; sort: KeySort; needsRun: boolean; along: string | null; rangeValid: boolean }): string {
  const out = new URLSearchParams()
  if (p.filter.states.length > 0) out.set('state', p.filter.states.join(','))
  if (p.needsRun) out.set('needs_run', '1')
  for (const [name, values] of Object.entries(p.filter.values)) {
    if (values.length > 0) out.append('value', `${name}=${values.join(',')}`)
  }
  if (p.filter.range && p.along && p.rangeValid) out.set('range', `${p.along}=${p.filter.range.from}..${p.filter.range.to}`)
  if (p.filter.error !== null) out.set('error', String(p.filter.error))
  if (p.filter.q.trim() !== '') out.set('q', p.filter.q.trim())
  if (p.sort !== 'key') out.set('sort', p.sort)
  return out.toString()
}
