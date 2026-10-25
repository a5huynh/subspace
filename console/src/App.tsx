import { Chat } from './components/Chat'
import { WorkerForm } from './components/WorkerForm'
import { WorkerList } from './components/WorkerList'
import { href, useRoute } from './route'
import { useWorkers } from './useWorkers'

export default function App() {
  const route = useRoute()
  const { workers, error, refresh } = useWorkers()
  const onChanged = refresh

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
        <section className="panel">
          <p className="muted">{workers ? 'No such worker.' : 'Loading…'}</p>
        </section>
      )
      break
    }
    case 'home':
      main = (
        <section className="panel">
          <h2>Workers</h2>
          <p className="muted">
            Pick a worker on the left, or <a href={href({ page: 'new' })}>create one</a>.
          </p>
        </section>
      )
      break
  }

  return (
    <div className="layout">
      <WorkerList workers={workers} error={error} route={route} />
      <main className="main">{main}</main>
    </div>
  )
}
