import { useEffect, useReducer, useState } from 'react'

import { api, openChannelStream } from '../api/client'
import type { ChannelDetail } from '../api/generated/ChannelDetail'
import type { ChannelEvent } from '../api/generated/ChannelEvent'
import { applyChannelEvent, emptyChannel, fromDetail, withMeta, type ChannelState } from './channel'

type Action =
  | { type: 'detail'; detail: ChannelDetail }
  | { type: 'meta'; detail: ChannelDetail }
  | { type: 'event'; event: ChannelEvent }

function reducer(state: ChannelState, action: Action): ChannelState {
  switch (action.type) {
    case 'detail':
      return fromDetail(action.detail)
    case 'meta':
      return withMeta(state, action.detail)
    case 'event':
      return applyChannelEvent(state, action.event)
  }
}

const RETRY_MS = 2000

/**
 * Live state for one channel: fetch its detail, stream events from its
 * cursor, refetch name/topic/members when they change, and on `resync`
 * refetch everything and reopen.
 */
export function useChannel(id: string) {
  const [state, dispatch] = useReducer(reducer, emptyChannel)
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
        const detail = await api.getChannel(id)
        if (cancelled) return
        setError(null)
        dispatch({ type: 'detail', detail })
        close = openChannelStream(id, detail, {
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
    api.getChannel(id).then(
      (detail) => active && dispatch({ type: 'meta', detail }),
      () => {},
    )
    return () => {
      active = false
    }
  }, [id, state.stale, state.deleted])

  return { state, connected, error }
}
