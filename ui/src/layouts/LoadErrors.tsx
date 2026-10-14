import { useHealth } from '@/hooks/useHealth'

/** Source failures are server diagnostics, never synthetic graph nodes. */
export function LoadErrors() {
  const { data } = useHealth()
  const errors = data?.load_errors ?? []
  if (!errors.length) return null
  return (
    <aside role="alert" style={{ padding: '12px 20px', borderBottom: '1px solid var(--border-default)', color: 'var(--text-default)' }}>
      <strong>Some source files or definitions could not be loaded.</strong>
      <ul>
        {errors.map((error, index) => (
          <li key={`${error.file}:${index}`}>
            <code>{error.file}</code>: {error.error}
            {error.affected_nodes.length > 0 && <div>Not loaded: {error.affected_nodes.join(', ')}</div>}
          </li>
        ))}
      </ul>
    </aside>
  )
}
