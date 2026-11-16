import { useEffect, useMemo } from 'react'
import { useLocation } from 'react-router'
import { useGroups } from './useGroups'
import { documentTitle } from '@/lib/documentTitle'

/** Keep the tab's title on what is open (`lib/documentTitle.ts`). */
export function useDocumentTitle() {
  const { pathname, search } = useLocation()
  const groups = useGroups().data
  const names = useMemo(() => new Map((groups ?? []).map((g) => [g.id, g.name])), [groups])
  const title = documentTitle(pathname, new URLSearchParams(search), (id) => names.get(id) ?? null)
  useEffect(() => {
    document.title = title
  }, [title])
}
