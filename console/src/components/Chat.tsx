import { useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent } from 'react'

import { api } from '../api/client'
import type { WorkerSummary } from '../api/generated/WorkerSummary'
import { isToolResultMessage, toolResults } from '../chat/state'
import { useWorkerChat } from '../chat/useWorkerChat'
import { href } from '../route'
import { LiveView, MessageView } from './MessageView'
import { StatusBadge } from './StatusBadge'

export function Chat({ worker }: { worker: WorkerSummary }) {
  const { state, connected, error } = useWorkerChat(worker.id)
  const status = state.status ?? worker.status
  const results = toolResults(state)
  const scroller = useScrollToBottom([state.messages, state.live, state.notices])

  return (
    <section className="chat">
      <header className="chat-header">
        <div>
          <h2>{worker.name}</h2>
          {worker.description && <p className="muted small">{worker.description}</p>}
        </div>
        <div className="chat-header-right">
          <StatusBadge status={status} />
          <span className={connected ? 'conn conn-on' : 'conn conn-off'}>
            {connected ? 'live' : 'connecting…'}
          </span>
          <a className="button secondary" href={href({ page: 'settings', id: worker.id })}>
            Settings
          </a>
        </div>
      </header>

      <div className="messages" ref={scroller}>
        {error && <p className="error small">{error}</p>}
        {state.messages.length === 0 && !state.live && (
          <p className="muted empty">Say hello to {worker.name}.</p>
        )}
        {state.messages
          .filter((m) => !isToolResultMessage(m))
          .map((m) => (
            <MessageView key={m.id} message={m} results={results} />
          ))}
        {state.live && <LiveView blocks={state.live} results={results} />}
        {state.notices.map((n) => (
          <p key={n.seq} className={`notice notice-${n.level}`}>
            {n.text}
          </p>
        ))}
      </div>

      <Composer workerId={worker.id} busy={status?.state === 'running'} />
    </section>
  )
}

function Composer({ workerId, busy }: { workerId: string; busy: boolean }) {
  const [text, setText] = useState('')
  const [sending, setSending] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const send = async () => {
    const value = text.trim()
    if (!value || sending) return
    setSending(true)
    setError(null)
    try {
      await api.prompt(workerId, value)
      setText('')
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      setSending(false)
    }
  }

  const onKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === 'Enter' && !e.shiftKey && !e.nativeEvent.isComposing) {
      e.preventDefault()
      void send()
    }
  }

  return (
    <form
      className="composer"
      onSubmit={(e) => {
        e.preventDefault()
        void send()
      }}
    >
      {error && <p className="error small">{error}</p>}
      <textarea
        value={text}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={onKeyDown}
        placeholder={busy ? 'Working… (messages queue up)' : 'Message (Enter to send, Shift+Enter for a new line)'}
        rows={3}
        aria-label="Message"
      />
      <button type="submit" disabled={sending || !text.trim()}>
        Send
      </button>
    </form>
  )
}

/** Keep a scroll container pinned to the bottom while the user is near it. */
function useScrollToBottom(deps: unknown[]) {
  const ref = useRef<HTMLDivElement>(null)
  const pinned = useRef(true)

  useEffect(() => {
    const el = ref.current
    if (!el) return
    const onScroll = () => {
      pinned.current = el.scrollHeight - el.scrollTop - el.clientHeight < 80
    }
    el.addEventListener('scroll', onScroll)
    return () => el.removeEventListener('scroll', onScroll)
  }, [])

  useLayoutEffect(() => {
    const el = ref.current
    if (el && pinned.current) el.scrollTop = el.scrollHeight
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps)

  return ref
}
