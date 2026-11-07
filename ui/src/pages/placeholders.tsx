import { Book } from 'lucide-react'
import { useNavigate } from 'react-router'
import { usePageTopbarRun } from '@/hooks/useTopbarRun'
import { EmptyState } from './EmptyState'

export function DocsPage() {
  const navigate = useNavigate()
  usePageTopbarRun((handle) => navigate(`/runs?run=${encodeURIComponent(handle)}`))
  return <EmptyState icon={Book} title="Docs" note="getting started · coming soon" />
}
