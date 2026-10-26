import { useCallback, useEffect, useState, type FormEvent } from 'react'

import { api } from '../api/client'
import type { ChannelDetail } from '../api/generated/ChannelDetail'
import type { ChannelMember } from '../api/generated/ChannelMember'
import type { WorkerSummary } from '../api/generated/WorkerSummary'
import type { WorkspaceDetail } from '../api/generated/WorkspaceDetail'
import { href, navigate } from '../route'

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
    <section className="panel">
      <h2>New channel</h2>
      <form className="form" onSubmit={submit}>
        <label>
          Name
          <input
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder="e.g. launch-plan"
            required
            autoFocus
          />
        </label>
        <label>
          Topic
          <input
            value={topic}
            onChange={(e) => setTopic(e.target.value)}
            placeholder="What this channel is working on"
          />
        </label>
        <fieldset className="checklist">
          <legend>Members</legend>
          {candidates.length === 0 && (
            <p className="muted small">
              No workers yet. <a href={href({ page: 'new' })}>Create one</a> first.
            </p>
          )}
          {candidates.map((w) => (
            <label key={w.id} className="check">
              <input
                type="checkbox"
                checked={members.includes(w.id)}
                onChange={() => toggle(w.id)}
              />
              <span>{w.name}</span>
              {w.description && <span className="muted small">{w.description}</span>}
            </label>
          ))}
        </fieldset>

        {error && <p className="error">{error}</p>}
        <div className="actions">
          <button type="submit" disabled={saving || !workspace}>
            Create channel
          </button>
        </div>
      </form>
    </section>
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
      <section className="panel">
        {error ? <p className="error">{error}</p> : <p className="muted">Loading…</p>}
      </section>
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
    <section className="panel">
      <h2>#{name} settings</h2>
      <form className="form" onSubmit={save}>
        <label>
          Name
          <input value={name} onChange={(e) => setName(e.target.value)} required />
        </label>
        <label>
          Topic
          <input value={topic} onChange={(e) => setTopic(e.target.value)} />
        </label>
        <div className="actions">
          <button type="submit">Save</button>
          <a className="button secondary" href={href({ page: 'channel', id })}>
            Back to channel
          </a>
        </div>
      </form>

      <h3>Members</h3>
      <ul className="member-list">
        <li>
          <span>You</span>
          <span className="muted small">@you · always here</span>
        </li>
        {members.map((m) => (
          <li key={m.worker_id}>
            <span>{m.name}</span>
            <span className="muted small">@{m.handle}</span>
            <button
              type="button"
              className="secondary small-button"
              onClick={() => void run(() => api.removeChannelMember(id, m.worker_id))}
            >
              Remove
            </button>
          </li>
        ))}
      </ul>
      {addable.length > 0 && (
        <div className="add-member">
          <select value={adding} onChange={(e) => setAdding(e.target.value)} aria-label="Add a member">
            <option value="">Add a worker…</option>
            {addable.map((w) => (
              <option key={w.id} value={w.id}>
                {w.name}
              </option>
            ))}
          </select>
          <button
            type="button"
            disabled={!adding}
            onClick={() =>
              void run(() => api.addChannelMember(id, adding)).then((ok) => ok && setAdding(''))
            }
          >
            Add
          </button>
        </div>
      )}

      {error && <p className="error">{error}</p>}
      {saved && <p className="success">Saved.</p>}

      <div className="actions danger-zone">
        <button type="button" className="danger" onClick={() => void remove()}>
          Delete channel
        </button>
      </div>
    </section>
  )
}
