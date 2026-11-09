/**
 * The key explorer's address: which node, which key is open, and which keys are shown. The URL is
 * the page's model, so a filtered view or an open key is a link someone can paste.
 *
 * `#/keys?node=p.py:events&key=day=2026-01-01,region=apac&state=failed,blocked&v.region=apac&error=0&q=03-`
 */

import { KEY_STATES, NO_FILTER, type KeyFilter, type KeyState } from './partitionGrid'

export interface KeysParams {
  node: string | null
  key: string | null
  filter: KeyFilter
  /** Every key in the table, not only those that need attention (`show=all`). */
  all: boolean
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
  }
}

/** The query string of an explorer view (without `?`); empty parts are left out. */
export function keysSearch(p: KeysParams): string {
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
  return out.toString()
}

/** The explorer of `node`, optionally at one key or with a filter: the link other pages use. */
export function keysHref(node: string, extra: { key?: string; filter?: Partial<KeyFilter> } = {}): string {
  return `/keys?${keysSearch({ node, key: extra.key ?? null, filter: { ...NO_FILTER, ...extra.filter }, all: false })}`
}

/** A state chip pressed: it is added to the shown states, or taken out of them. */
export function toggleState(f: KeyFilter, state: KeyState): KeyFilter {
  const states = f.states.includes(state) ? f.states.filter((s) => s !== state) : [...f.states, state]
  return { ...f, states: KEY_STATES.filter((s) => states.includes(s)) }
}

/** One dimension's values set (none: every value). */
export function withValues(f: KeyFilter, dim: string, values: readonly string[]): KeyFilter {
  const next = { ...f.values }
  if (values.length === 0) delete next[dim]
  else next[dim] = [...values]
  return { ...f, values: next }
}
