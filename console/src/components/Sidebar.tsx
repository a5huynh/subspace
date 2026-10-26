import type { WorkerSummary } from '../api/generated/WorkerSummary'
import type { WorkspaceDetail } from '../api/generated/WorkspaceDetail'
import { href, type Route } from '../route'

interface Props {
  workspace: WorkspaceDetail | null
  workers: WorkerSummary[] | null
  error: string | null
  route: Route
}

export function Sidebar({ workspace, workers, error, route }: Props) {
  const selectedWorker = route.page === 'worker' || route.page === 'settings' ? route.id : null
  const selectedChannel =
    route.page === 'channel' || route.page === 'channel-settings' ? route.id : null
  const channels = workspace?.channels ?? []

  return (
    <nav className="sidebar">
      <div className="sidebar-header">
        <a className="brand" href={href({ page: 'home' })}>
          Subspace
        </a>
        {workspace && <span className="workspace-name">{workspace.workspace.name}</span>}
      </div>
      {error && <p className="error small">Can't reach the server: {error}</p>}

      <SectionHeader title="Channels" add={href({ page: 'new-channel' })} addLabel="New channel" />
      {workspace && channels.length === 0 && <p className="muted small indent">No channels yet.</p>}
      <ul className="nav-list">
        {channels.map((c) => (
          <li key={c.id}>
            <a
              href={href({ page: 'channel', id: c.id })}
              className={c.id === selectedChannel ? 'selected' : undefined}
            >
              <span className="hash">#</span>
              <span className="nav-name">{c.name}</span>
            </a>
          </li>
        ))}
      </ul>

      <SectionHeader title="Direct messages" add={href({ page: 'new' })} addLabel="New worker" />
      {workers?.length === 0 && <p className="muted small indent">No workers yet.</p>}
      <ul className="nav-list">
        {workers?.map((w) => (
          <li key={w.id}>
            <a
              href={href({ page: 'worker', id: w.id })}
              className={w.id === selectedWorker ? 'selected' : undefined}
            >
              <span className={`dot dot-${w.status.state}`} aria-label={w.status.state} />
              <span className="nav-name">{w.name}</span>
              {w.description && <span className="nav-desc">{w.description}</span>}
            </a>
          </li>
        ))}
      </ul>
    </nav>
  )
}

function SectionHeader({ title, add, addLabel }: { title: string; add: string; addLabel: string }) {
  return (
    <div className="section-header">
      <span>{title}</span>
      <a className="section-add" href={add} aria-label={addLabel} title={addLabel}>
        +
      </a>
    </div>
  )
}
