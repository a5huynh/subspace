import { useEffect, useReducer, useState } from 'react'

import { api, openWorkerStream } from '../api/client'
import type { Message } from '../api/generated/Message'
import type { ServerEvent } from '../api/generated/ServerEvent'
import { applyEvent, emptyChat, fromHistory, type ChatState } from './state'

type Action = { type: 'history'; messages: Message[] } | { type: 'event'; event: ServerEvent }

function reducer(state: ChatState, action: Action): ChatState {
  return action.type === 'history'
    ? fromHistory(state, action.messages)
    : applyEvent(state, action.event)
}

const RETRY_MS = 2000

/**
 * Live chat state for one worker: fetch history, stream events from its
 * cursor, and on `resync` refetch and reopen. Reconnects are handled by the
 * stream; a failed history fetch retries.
 */
export function useWorkerChat(id: string) {
  const [state, dispatch] = useReducer(reducer, emptyChat)
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
        const history = await api.history(id)
        if (cancelled) return
        setError(null)
        dispatch({ type: 'history', messages: history.messages })
        close = openWorkerStream(id, history, {
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

  return { state, connected, error }
}
