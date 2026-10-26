import type { ReactNode } from 'react'

import type { WorkerSummary } from '../api/generated/WorkerSummary'
import type { WorkspaceDetail } from '../api/generated/WorkspaceDetail'
import { href, type Route } from '../route'
import { Avatar } from './ui'

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
    <nav className="flex min-h-0 flex-col bg-sidebar text-sidebar-muted">
      <a
        href={href({ page: 'home' })}
        className="flex h-[57px] shrink-0 items-center gap-2 border-b border-sidebar-line px-4 text-lg font-black text-sidebar-ink no-underline hover:no-underline"
      >
        <span className="truncate">{workspace?.workspace.name ?? 'Subspace'}</span>
        <span className="text-xs font-normal text-sidebar-muted">Subspace</span>
      </a>

      <div className="min-h-0 flex-1 overflow-y-auto px-2 py-3">
        {error && (
          <p className="mx-2 mb-2 rounded bg-danger/20 px-2 py-1 text-xs text-sidebar-ink">
            Can't reach the server: {error}
          </p>
        )}

        <Section title="Channels" add={href({ page: 'new-channel' })} addLabel="Create a channel">
          {channels.map((c) => (
            <Item key={c.id} href={href({ page: 'channel', id: c.id })} selected={c.id === selectedChannel}>
              <span className="w-5 text-center opacity-70">#</span>
              <span className="truncate">{c.name}</span>
            </Item>
          ))}
          {workspace && channels.length === 0 && (
            <Item href={href({ page: 'new-channel' })}>
              <span className="w-5 text-center">+</span>
              <span>Add a channel</span>
            </Item>
          )}
        </Section>

        <Section title="Direct messages" add={href({ page: 'new' })} addLabel="Create a worker">
          {workers?.map((w) => (
            <Item
              key={w.id}
              href={href({ page: 'worker', id: w.id })}
              selected={w.id === selectedWorker}
              title={w.description || undefined}
            >
              <Avatar name={w.name} size="xs" status={w.status} />
              <span className="truncate">{w.name}</span>
            </Item>
          ))}
          {workers?.length === 0 && (
            <Item href={href({ page: 'new' })}>
              <span className="w-5 text-center">+</span>
              <span>Add a worker</span>
            </Item>
          )}
        </Section>
      </div>
    </nav>
  )
}

function Section({
  title,
  add,
  addLabel,
  children,
}: {
  title: string
  add: string
  addLabel: string
  children: ReactNode
}) {
  return (
    <div className="mb-4">
      <div className="group flex items-center justify-between px-2 py-1 text-[15px]">
        <span>{title}</span>
        <a
          href={add}
          aria-label={addLabel}
          title={addLabel}
          className="flex size-6 items-center justify-center rounded text-lg leading-none text-sidebar-muted no-underline opacity-70 group-hover:opacity-100 hover:bg-sidebar-hover hover:text-sidebar-ink hover:no-underline"
        >
          +
        </a>
      </div>
      <ul>{children}</ul>
    </div>
  )
}

function Item({
  href: to,
  selected,
  title,
  children,
}: {
  href: string
  selected?: boolean
  title?: string
  children: ReactNode
}) {
  return (
    <li>
      <a
        href={to}
        title={title}
        aria-current={selected ? 'page' : undefined}
        className={`flex h-7 items-center gap-2 rounded-md px-2 text-[15px] no-underline hover:no-underline ${
          selected
            ? 'bg-sidebar-active font-bold text-white'
            : 'text-sidebar-muted hover:bg-sidebar-hover hover:text-sidebar-ink'
        }`}
      >
        {children}
      </a>
    </li>
  )
}
