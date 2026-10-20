import type { ListNode } from '@/lib/types'

/**
 * Placeholder asset graph used while the server is unreachable.
 * Mirrors the design system's revenue_daily pipeline.
 */
export const mockAssets: ListNode[] = [
  { id: 'pipeline.py:checkout_events', kind: 'asset', freshness: 'always', env: [], inputs: [] },
  { id: 'pipeline.py:product_catalog', kind: 'asset', freshness: 'always', env: [], inputs: [] },
  { id: 'pipeline.py:fx_rates', kind: 'asset', freshness: 'schedule', schedule: '0 */2 * * *', env: [], inputs: [] },
  { id: 'pipeline.py:sessionize', kind: 'asset', freshness: 'always', env: [], inputs: ['pipeline.py:checkout_events'] },
  { id: 'pipeline.py:orders_clean', kind: 'asset', freshness: 'always', env: [], inputs: ['pipeline.py:checkout_events', 'pipeline.py:product_catalog'] },
  { id: 'pipeline.py:orders_enriched', kind: 'asset', freshness: 'always', env: [], inputs: ['pipeline.py:sessionize', 'pipeline.py:orders_clean', 'pipeline.py:fx_rates'] },
  { id: 'pipeline.py:revenue_daily', kind: 'asset', freshness: 'always', env: [], inputs: ['pipeline.py:orders_enriched'] },
  { id: 'pipeline.py:daily_report', kind: 'task', freshness: 'manual', env: [], inputs: ['pipeline.py:orders_clean'] },
]
