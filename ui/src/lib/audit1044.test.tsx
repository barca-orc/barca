import { expect, it } from 'vitest'
import { renderToStaticMarkup } from 'react-dom/server'
import { KeyTable } from '@/components/partitions/KeyTable'
import { nodeEstimate } from './estimates'
import { keysParams, keysQuery } from './keysPage'

it('keeps an empty-key filter in the request sent to the server', () => {
  const { filter } = keysParams(new URLSearchParams('empty=1'))
  expect(new URLSearchParams(keysQuery({ filter, sort: 'rows', needsRun: false, along: null, rangeValid: false })).get('empty')).toBe('1')
})
it('shows a Rows column in the key explorer', () => {
  const html = renderToStaticMarkup(<KeyTable dimensions={['day']} rows={[]} total={0} selected={null} onSelect={() => {}} onNeedMore={() => {}} loadingMore={false} sort="key" onSort={() => {}} query="" stale={false} nowMs={0} />)
  expect(html).toContain('Rows')
})
it('never leaves the Fresh by cell blank for a fresh node', () => {
  expect(nodeEstimate({ expected_fresh_at: null, recheck_at: null, no_estimate: null, cause: null }, 'utc', 0).text).toBe('fresh')
})
