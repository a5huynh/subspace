import { WorkspaceSettings, NewWorkspaceForm } from './components/WorkspaceForm'
import { WorkspaceView } from './components/WorkspaceView'
import { Chat } from './components/Chat'
import { Sidebar } from './components/Sidebar'
import { WorkerForm } from './components/WorkerForm'
import { EmptyState, Panel } from './components/ui'
import { href, useRoute } from './route'
import { useWorkers } from './useWorkers'
import { useTeam } from './useTeam'

export default function App() {
  const route = useRoute()
  const { workers, error, refresh: refreshWorkers } = useWorkers()
  const { team, refresh: refreshTeam } = useTeam()
  const onChanged = () => {
    refreshWorkers()
    refreshTeam()
  }

  let main
  switch (route.page) {
    case 'new':
      main = <WorkerForm onChanged={onChanged} />
      break
    case 'settings':
      main = <WorkerForm key={route.id} id={route.id} onChanged={onChanged} />
      break
    case 'worker': {
      const worker = workers?.find((w) => w.id === route.id)
      main = worker ? (
        <Chat key={worker.id} worker={worker} />
      ) : (
        <EmptyState>{workers ? 'No such worker.' : 'Loading…'}</EmptyState>
      )
      break
    }
    case 'new-workspace':
      main = <NewWorkspaceForm team={team} workers={workers} onChanged={onChanged} />
      break
    case 'workspace':
      main = <WorkspaceView key={route.id} id={route.id} />
      break
    case 'workspace-settings':
      main = (
        <WorkspaceSettings
          key={route.id}
          id={route.id}
          team={team}
          workers={workers}
          onChanged={onChanged}
        />
      )
      break
    case 'home':
      main = (
        <Panel title={`Welcome to ${team?.team.name ?? 'Subspace'}`}>
          <p className="text-muted">
            Open a workspace to work with several workers together, or a direct message to talk to
            one. <a href={href({ page: 'new-workspace' })}>Create a workspace</a> or{' '}
            <a href={href({ page: 'new' })}>a worker</a>.
          </p>
        </Panel>
      )
      break
  }

  return (
    <div className="grid h-full grid-cols-[260px_minmax(0,1fr)]">
      <Sidebar team={team} workers={workers} error={error} route={route} />
      <main className="flex min-h-0 flex-col">{main}</main>
    </div>
  )
}
