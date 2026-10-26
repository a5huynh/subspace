import { useEffect, useReducer, useState } from 'react'

import { api, openWorkspaceStream } from '../api/client'
import type { WorkspaceDetail } from '../api/generated/WorkspaceDetail'
import type { WorkspaceEvent } from '../api/generated/WorkspaceEvent'
import { applyWorkspaceEvent, emptyWorkspace, fromDetail, withMeta, type WorkspaceState } from './workspace'

type Action =
  | { type: 'detail'; detail: WorkspaceDetail }
  | { type: 'meta'; detail: WorkspaceDetail }
  | { type: 'event'; event: WorkspaceEvent }

function reducer(state: WorkspaceState, action: Action): WorkspaceState {
  switch (action.type) {
    case 'detail':
      return fromDetail(action.detail)
    case 'meta':
      return withMeta(state, action.detail)
    case 'event':
      return applyWorkspaceEvent(state, action.event)
  }
}

const RETRY_MS = 2000

/**
 * Live state for one workspace: fetch its detail, stream events from its
 * cursor, refetch name/topic/members when they change, and on `resync`
 * refetch everything and reopen.
 */
export function useWorkspace(id: string) {
  const [state, dispatch] = useReducer(reducer, emptyWorkspace)
  const [connected, setConnected] = useState(false)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    let cancelled = false
    let close: (() => void) | null = null
    let retry: ReturnType<typeof setTimeout> | undefined

    const load = async () => {
      close?.()
      close = null
      try {
        const detail = await api.getWorkspace(id)
        if (cancelled) return
        setError(null)
        dispatch({ type: 'detail', detail })
        close = openWorkspaceStream(id, detail, {
          onEvent: (event) => dispatch({ type: 'event', event }),
          onResync: () => void load(),
          onConnectionChange: setConnected,
        })
      } catch (e) {
        if (cancelled) return
        setError(e instanceof Error ? e.message : String(e))
        retry = setTimeout(() => void load(), RETRY_MS)
      }
    }

    void load()
    return () => {
      cancelled = true
      clearTimeout(retry)
      close?.()
      setConnected(false)
    }
  }, [id])

  // Refetch name, topic, and members when the stream says they changed.
  useEffect(() => {
    if (!state.stale || state.deleted) return
    let active = true
    api.getWorkspace(id).then(
      (detail) => active && dispatch({ type: 'meta', detail }),
      () => {},
    )
    return () => {
      active = false
    }
  }, [id, state.stale, state.deleted])

  return { state, connected, error }
}
