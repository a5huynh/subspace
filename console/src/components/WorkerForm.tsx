import { useEffect, useState, type FormEvent } from 'react'

import { ApiRequestError, api } from '../api/client'
import type { ThinkingLevel } from '../api/generated/ThinkingLevel'
import type { WorkerConfig } from '../api/generated/WorkerConfig'
import { navigate } from '../route'
import { Panel } from './ui'

const THINKING: ThinkingLevel[] = ['off', 'low', 'medium', 'high']

interface Fields {
  name: string
  description: string
  role: string
  model: string
  thinking: ThinkingLevel
  disabledSkills: string
}

const blank: Fields = {
  name: '',
  description: '',
  role: '',
  model: '',
  thinking: 'low',
  disabledSkills: '',
}

function fieldsOf(cfg: WorkerConfig): Fields {
  return {
    name: cfg.name,
    description: cfg.description,
    role: cfg.role,
    model: cfg.model,
    thinking: cfg.thinking,
    disabledSkills: cfg.skills.disabled.join(', '),
  }
}

function parseSkills(s: string): string[] {
  return s
    .split(',')
    .map((x) => x.trim())
    .filter(Boolean)
}

/** Create a worker (no `id`) or edit an existing one's settings. */
export function WorkerForm({ id, onChanged }: { id?: string; onChanged: () => void }) {
  const editing = id !== undefined
  const [fields, setFields] = useState<Fields | null>(editing ? null : blank)
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [saved, setSaved] = useState(false)

  useEffect(() => {
    if (!id) return
    let cancelled = false
    api.getWorker(id).then(
      (w) => !cancelled && setFields(fieldsOf(w.config)),
      (e: unknown) => !cancelled && setError(e instanceof Error ? e.message : String(e)),
    )
    return () => {
      cancelled = true
    }
  }, [id])

  if (!fields) {
    return (
      <Panel title="Worker settings">
        {error ? <p className="text-danger">{error}</p> : <p className="text-muted">Loading…</p>}
      </Panel>
    )
  }

  const set = <K extends keyof Fields>(key: K, value: Fields[K]) => {
    setFields({ ...fields, [key]: value })
    setSaved(false)
  }

  const submit = async (e: FormEvent) => {
    e.preventDefault()
    setSaving(true)
    setError(null)
    const body = {
      name: fields.name,
      description: fields.description,
      role: fields.role,
      thinking: fields.thinking,
      skills: { disabled: parseSkills(fields.disabledSkills) },
      ...(fields.model.trim() ? { model: fields.model } : {}),
    }
    try {
      if (editing) {
        const w = await api.updateWorker(id, body)
        setFields(fieldsOf(w.config))
        setSaved(true)
      } else {
        const w = await api.createWorker(body)
        navigate({ page: 'worker', id: w.config.id })
      }
      onChanged()
    } catch (err) {
      setError(
        err instanceof ApiRequestError && err.status === 409
          ? `${err.message}. Wait for the current run to finish, then save again.`
          : err instanceof Error
            ? err.message
            : String(err),
      )
    } finally {
      setSaving(false)
    }
  }

  const remove = async () => {
    if (!id || !confirm(`Delete ${fields.name}? This removes its home, memory, and history.`)) return
    try {
      await api.deleteWorker(id)
      onChanged()
      navigate({ page: 'home' })
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err))
    }
  }

  return (
    <Panel title={editing ? `${fields.name} settings` : 'New worker'}>
      <form className="flex flex-col gap-5" onSubmit={submit}>
        <label className="label">
          Name
          <input className="field" value={fields.name} onChange={(e) => set('name', e.target.value)} required autoFocus={!editing} />
        </label>
        <label className="label">
          Description
          <input
            className="field"
            value={fields.description}
            onChange={(e) => set('description', e.target.value)}
            placeholder="One line: what this worker is good at"
          />
        </label>
        <label className="label">
          Role
          <textarea
            className="field font-mono text-sm"
            value={fields.role}
            onChange={(e) => set('role', e.target.value)}
            rows={10}
            placeholder="Persona and standing instructions (saved as ROLE.md)"
          />
        </label>
        <div className="grid grid-cols-[2fr_1fr] gap-4">
          <label className="label">
            Model
            <input
            className="field"
              value={fields.model}
              onChange={(e) => set('model', e.target.value)}
              placeholder={editing ? undefined : 'Server default'}
              required={editing}
            />
          </label>
          <label className="label">
            Thinking
            <select className="field" value={fields.thinking} onChange={(e) => set('thinking', e.target.value as ThinkingLevel)}>
              {THINKING.map((t) => (
                <option key={t} value={t}>
                  {t}
                </option>
              ))}
            </select>
          </label>
        </div>
        <label className="label">
          Disabled skills
          <input
            className="field"
            value={fields.disabledSkills}
            onChange={(e) => set('disabledSkills', e.target.value)}
            placeholder="Comma-separated skill names (all others are enabled)"
          />
        </label>

        {error && <p className="text-danger">{error}</p>}
        {saved && <p className="text-send">Saved. Changes apply to the worker's next session.</p>}

        <div className="flex items-center gap-2.5">
          <button type="submit" className="btn-primary" disabled={saving}>
            {editing ? 'Save' : 'Create worker'}
          </button>
          {editing && (
            <>
              <a className="btn-secondary" href={`#/workers/${encodeURIComponent(id)}`}>
                Back to chat
              </a>
              <button type="button" className="btn-danger ml-auto" onClick={() => void remove()}>
                Delete worker
              </button>
            </>
          )}
        </div>
      </form>
    </Panel>
  )
}
