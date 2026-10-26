import { ChannelSettings, NewChannelForm } from './components/ChannelForm'
import { ChannelView } from './components/ChannelView'
import { Chat } from './components/Chat'
import { Sidebar } from './components/Sidebar'
import { WorkerForm } from './components/WorkerForm'
import { EmptyState, Panel } from './components/ui'
import { href, useRoute } from './route'
import { useWorkers } from './useWorkers'
import { useWorkspace } from './useWorkspace'

export default function App() {
  const route = useRoute()
  const { workers, error, refresh: refreshWorkers } = useWorkers()
  const { workspace, refresh: refreshWorkspace } = useWorkspace()
  const onChanged = () => {
    refreshWorkers()
    refreshWorkspace()
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
    case 'new-channel':
      main = <NewChannelForm workspace={workspace} workers={workers} onChanged={onChanged} />
      break
    case 'channel':
      main = <ChannelView key={route.id} id={route.id} />
      break
    case 'channel-settings':
      main = (
        <ChannelSettings
          key={route.id}
          id={route.id}
          workspace={workspace}
          workers={workers}
          onChanged={onChanged}
        />
      )
      break
    case 'home':
      main = (
        <Panel title={`Welcome to ${workspace?.workspace.name ?? 'Subspace'}`}>
          <p className="text-muted">
            Open a channel to work with several workers together, or a direct message to talk to
            one. <a href={href({ page: 'new-channel' })}>Create a channel</a> or{' '}
            <a href={href({ page: 'new' })}>a worker</a>.
          </p>
        </Panel>
      )
      break
  }

  return (
    <div className="grid h-full grid-cols-[260px_minmax(0,1fr)]">
      <Sidebar workspace={workspace} workers={workers} error={error} route={route} />
      <main className="flex min-h-0 flex-col">{main}</main>
    </div>
  )
}
