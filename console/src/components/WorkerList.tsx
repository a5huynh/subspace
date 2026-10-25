import type { WorkerSummary } from '../api/generated/WorkerSummary'
import { href, type Route } from '../route'

interface Props {
  workers: WorkerSummary[] | null
  error: string | null
  route: Route
}

export function WorkerList({ workers, error, route }: Props) {
  const selected = route.page === 'worker' || route.page === 'settings' ? route.id : null
  return (
    <nav className="sidebar">
      <div className="sidebar-header">
        <a className="brand" href={href({ page: 'home' })}>
          Subspace
        </a>
        <a className="button" href={href({ page: 'new' })}>
          + New worker
        </a>
      </div>
      {error && <p className="error small">Can't reach the server: {error}</p>}
      {workers?.length === 0 && <p className="muted small">No workers yet.</p>}
      <ul className="worker-list">
        {workers?.map((w) => (
          <li key={w.id}>
            <a
              href={href({ page: 'worker', id: w.id })}
              className={w.id === selected ? 'selected' : undefined}
            >
              <span className={`dot dot-${w.status.state}`} aria-label={w.status.state} />
              <span className="worker-name">{w.name}</span>
              {w.description && <span className="worker-desc">{w.description}</span>}
            </a>
          </li>
        ))}
      </ul>
    </nav>
  )
}
