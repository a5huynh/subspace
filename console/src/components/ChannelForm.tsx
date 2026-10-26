import { useCallback, useEffect, useState, type FormEvent } from 'react'

import { api } from '../api/client'
import type { ChannelDetail } from '../api/generated/ChannelDetail'
import type { ChannelMember } from '../api/generated/ChannelMember'
import type { WorkerSummary } from '../api/generated/WorkerSummary'
import type { WorkspaceDetail } from '../api/generated/WorkspaceDetail'
import { href, navigate } from '../route'
import { Avatar, Panel } from './ui'

interface Props {
  workspace: WorkspaceDetail | null
  workers: WorkerSummary[] | null
  onChanged: () => void
}

const message = (e: unknown) => (e instanceof Error ? e.message : String(e))

/** Workers in the workspace, with their names, in sidebar order. */
function workspaceWorkers(workspace: WorkspaceDetail | null, workers: WorkerSummary[] | null) {
  const ids = new Set(workspace?.members ?? [])
  return (workers ?? []).filter((w) => ids.has(w.id))
}

export function NewChannelForm({ workspace, workers, onChanged }: Props) {
  const [name, setName] = useState('')
  const [topic, setTopic] = useState('')
  const [members, setMembers] = useState<string[]>([])
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const candidates = workspaceWorkers(workspace, workers)

  const submit = async (e: FormEvent) => {
    e.preventDefault()
    if (!workspace) return
    setSaving(true)
    setError(null)
    try {
      const channel = await api.createChannel(workspace.workspace.id, { name, topic, members })
      onChanged()
      navigate({ page: 'channel', id: channel.id })
    } catch (err) {
      setError(message(err))
    } finally {
      setSaving(false)
    }
  }

  const toggle = (id: string) =>
    setMembers((m) => (m.includes(id) ? m.filter((x) => x !== id) : [...m, id]))

  return (
    <Panel title="Create a channel">
      <form className="flex flex-col gap-5" onSubmit={submit}>
        <label className="label">
          Name
          <input
            className="field"
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder="e.g. launch-plan"
            required
            autoFocus
          />
        </label>
        <label className="label">
          Topic
          <input
            className="field"
            value={topic}
            onChange={(e) => setTopic(e.target.value)}
            placeholder="What this channel is working on"
          />
        </label>
        <fieldset className="flex flex-col gap-1 rounded-md border border-line p-3">
          <legend className="px-1 text-sm font-bold">Members</legend>
          {candidates.length === 0 && (
            <p className="text-sm text-muted">
              No workers yet. <a href={href({ page: 'new' })}>Create one</a> first.
            </p>
          )}
          {candidates.map((w) => (
            <label key={w.id} className="flex cursor-pointer items-center gap-2.5 rounded-md px-2 py-1.5 hover:bg-hover">
              <input
                type="checkbox"
                className="size-4 accent-send"
                checked={members.includes(w.id)}
                onChange={() => toggle(w.id)}
              />
              <Avatar name={w.name} size="sm" />
              <span className="font-bold">{w.name}</span>
              {w.description && <span className="truncate text-sm text-muted">{w.description}</span>}
            </label>
          ))}
        </fieldset>

        {error && <p className="text-danger">{error}</p>}
        <div>
          <button type="submit" className="btn-primary" disabled={saving || !workspace}>
            Create channel
          </button>
        </div>
      </form>
    </Panel>
  )
}

export function ChannelSettings({ id, workspace, workers, onChanged }: Props & { id: string }) {
  const [name, setName] = useState<string | null>(null)
  const [topic, setTopic] = useState('')
  const [members, setMembers] = useState<ChannelMember[]>([])
  const [adding, setAdding] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [saved, setSaved] = useState(false)

  const apply = useCallback((detail: ChannelDetail) => {
    setName(detail.channel.name)
    setTopic(detail.channel.topic)
    setMembers(detail.members)
  }, [])

  useEffect(() => {
    let cancelled = false
    api.getChannel(id).then(
      (detail) => !cancelled && apply(detail),
      (e: unknown) => !cancelled && setError(message(e)),
    )
    return () => {
      cancelled = true
    }
  }, [id, apply])

  if (name === null) {
    return (
      <Panel title="Channel settings">
        {error ? <p className="text-danger">{error}</p> : <p className="text-muted">Loading…</p>}
      </Panel>
    )
  }

  const run = async (action: () => Promise<unknown>) => {
    setError(null)
    setSaved(false)
    try {
      await action()
      apply(await api.getChannel(id))
      onChanged()
      return true
    } catch (err) {
      setError(message(err))
      return false
    }
  }

  const save = async (e: FormEvent) => {
    e.preventDefault()
    if (await run(() => api.updateChannel(id, { name, topic }))) setSaved(true)
  }

  const remove = async () => {
    if (!confirm(`Delete #${name}? This removes its posts and every member's session in it.`)) return
    try {
      await api.deleteChannel(id)
      onChanged()
      navigate({ page: 'home' })
    } catch (err) {
      setError(message(err))
    }
  }

  const memberIds = new Set(members.map((m) => m.worker_id))
  const addable = workspaceWorkers(workspace, workers).filter((w) => !memberIds.has(w.id))

  return (
    <Panel title={<># {name} settings</>}>
      <form className="flex flex-col gap-5" onSubmit={save}>
        <label className="label">
          Name
          <input className="field" value={name} onChange={(e) => setName(e.target.value)} required />
        </label>
        <label className="label">
          Topic
          <input className="field" value={topic} onChange={(e) => setTopic(e.target.value)} />
        </label>
        <div className="flex items-center gap-2.5">
          <button type="submit" className="btn-primary">
            Save
          </button>
          <a className="btn-secondary" href={href({ page: 'channel', id })}>
            Back to channel
          </a>
        </div>
      </form>

      <h2 className="mt-8 mb-2 text-lg font-black">Members</h2>
      <ul className="divide-y divide-line rounded-md border border-line">
        <li className="flex items-center gap-2.5 px-3 py-2">
          <Avatar name="You" size="sm" />
          <span className="font-bold">You</span>
          <span className="text-sm text-muted">@you · always here</span>
        </li>
        {members.map((m) => (
          <li key={m.worker_id} className="flex items-center gap-2.5 px-3 py-2">
            <Avatar name={m.name} size="sm" />
            <span className="font-bold">{m.name}</span>
            <span className="truncate text-sm text-muted">@{m.handle}</span>
            <button
              type="button"
              className="btn-secondary ml-auto py-0.5 text-xs"
              onClick={() => void run(() => api.removeChannelMember(id, m.worker_id))}
            >
              Remove
            </button>
          </li>
        ))}
      </ul>
      {addable.length > 0 && (
        <div className="mt-3 flex gap-2.5">
          <select
            className="field"
            value={adding}
            onChange={(e) => setAdding(e.target.value)}
            aria-label="Add a member"
          >
            <option value="">Add a worker…</option>
            {addable.map((w) => (
              <option key={w.id} value={w.id}>
                {w.name}
              </option>
            ))}
          </select>
          <button
            type="button"
            className="btn-primary"
            disabled={!adding}
            onClick={() =>
              void run(() => api.addChannelMember(id, adding)).then((ok) => ok && setAdding(''))
            }
          >
            Add
          </button>
        </div>
      )}

      {error && <p className="mt-3 text-danger">{error}</p>}
      {saved && <p className="mt-3 text-send">Saved.</p>}

      <div className="mt-10 border-t border-line pt-5">
        <button type="button" className="btn-danger" onClick={() => void remove()}>
          Delete channel
        </button>
      </div>
    </Panel>
  )
}
