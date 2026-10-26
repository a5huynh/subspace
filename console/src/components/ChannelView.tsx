import { useLayoutEffect, useMemo, useRef, useState, type KeyboardEvent } from 'react'

import { api } from '../api/client'
import type { ChannelMember } from '../api/generated/ChannelMember'
import type { ChannelPost } from '../api/generated/ChannelPost'
import { channelTimeline } from '../chat/channel'
import { OPERATOR_HANDLE, complete, mentionAt, segments, suggest } from '../chat/mentions'
import { isToolResultMessage, toolResults, type ChatState } from '../chat/state'
import { useChannel } from '../chat/useChannel'
import { href } from '../route'
import { useScrollToBottom } from '../useScrollToBottom'
import { LiveView, MessageView } from './MessageView'

export function ChannelView({ id }: { id: string }) {
  const { state, connected, error } = useChannel(id)
  const scroller = useScrollToBottom([state.posts, state.notices, state.running])
  const known = useMemo(
    () => new Set([OPERATOR_HANDLE, ...state.members.map((m) => m.handle)]),
    [state.members],
  )

  if (state.deleted) {
    return (
      <section className="panel">
        <p className="muted">
          This channel was deleted. <a href={href({ page: 'home' })}>Back home</a>
        </p>
      </section>
    )
  }

  const channel = state.channel
  const nameOf = (workerId: string) =>
    state.members.find((m) => m.worker_id === workerId)?.name ?? 'A worker'

  return (
    <section className="chat">
      <header className="chat-header">
        <div>
          <h2>
            <span className="hash">#</span>
            {channel?.name ?? '…'}
          </h2>
          <p className="muted small">
            {channel?.topic && <>{channel.topic} · </>}
            <MemberSummary members={state.members} />
          </p>
        </div>
        <div className="chat-header-right">
          <span className={connected ? 'conn conn-on' : 'conn conn-off'}>
            {connected ? 'live' : 'connecting…'}
          </span>
          <a className="button secondary" href={href({ page: 'channel-settings', id })}>
            Settings
          </a>
        </div>
      </header>

      <div className="messages" ref={scroller}>
        {error && <p className="error small">{error}</p>}
        {channel && state.posts.length === 0 && (
          <p className="muted empty">
            {state.members.length === 0
              ? 'Add workers in Settings, then @mention them to get them talking.'
              : `Start the conversation; @mention a member to bring them in.`}
          </p>
        )}
        {channelTimeline(state).map((item) =>
          item.kind === 'post' ? (
            <Post key={item.post.id} post={item.post} known={known} />
          ) : (
            <p key={item.notice.id} className={`notice notice-${item.notice.level}`}>
              {item.notice.text}
            </p>
          ),
        )}
        {Object.entries(state.running).map(([workerId, chat]) => (
          <Working key={workerId} name={nameOf(workerId)} chat={chat} />
        ))}
      </div>

      <ChannelComposer channelId={id} members={state.members} />
    </section>
  )
}

function MemberSummary({ members }: { members: ChannelMember[] }) {
  if (members.length === 0) return <>Just you</>
  return (
    <>
      You,{' '}
      {members.map((m, i) => (
        <span key={m.worker_id} title={m.description || m.name}>
          @{m.handle}
          {i < members.length - 1 ? ', ' : ''}
        </span>
      ))}
    </>
  )
}

function Post({ post, known }: { post: ChannelPost; known: Set<string> }) {
  const mine = post.author.kind === 'operator'
  return (
    <article className={mine ? 'post post-operator' : 'post'}>
      <header className="post-meta">
        <span className="post-author">{post.author_name}</span>
        <time className="muted small" dateTime={post.timestamp}>
          {new Date(post.timestamp).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })}
        </time>
      </header>
      <div className="text">
        {segments(post.text, known).map((s, i) =>
          s.kind === 'mention' ? (
            <span key={i} className={s.handle === OPERATOR_HANDLE ? 'mention mention-me' : 'mention'}>
              {s.text}
            </span>
          ) : (
            <span key={i}>{s.text}</span>
          ),
        )}
      </div>
    </article>
  )
}

/** A worker mid-turn: its finished steps (tool calls) and live output. */
function Working({ name, chat }: { name: string; chat: ChatState }) {
  const results = toolResults(chat)
  return (
    <article className="post post-live">
      <header className="post-meta">
        <span className="post-author">{name}</span>
        <span className="muted small typing">working…</span>
      </header>
      {chat.messages
        .filter((m) => m.role === 'assistant' && !isToolResultMessage(m))
        .map((m) => (
          <MessageView key={m.id} message={m} results={results} />
        ))}
      {chat.live && chat.live.length > 0 && <LiveView blocks={chat.live} results={results} />}
    </article>
  )
}

function ChannelComposer({ channelId, members }: { channelId: string; members: ChannelMember[] }) {
  const [text, setText] = useState('')
  const [caret, setCaret] = useState(0)
  const [highlight, setHighlight] = useState(0)
  const [dismissed, setDismissed] = useState(false)
  const [sending, setSending] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const input = useRef<HTMLTextAreaElement>(null)
  const pendingCaret = useRef<number | null>(null)

  const at = mentionAt(text, caret)
  const options = at && !dismissed ? suggest(members.map((m) => m.handle), at.query) : []
  const active = Math.min(highlight, Math.max(options.length - 1, 0))

  useLayoutEffect(() => {
    if (pendingCaret.current !== null && input.current) {
      input.current.setSelectionRange(pendingCaret.current, pendingCaret.current)
      pendingCaret.current = null
    }
  }, [text])

  const edit = (value: string, position: number) => {
    setText(value)
    setCaret(position)
    setHighlight(0)
    setDismissed(false)
  }

  const choose = (handle: string) => {
    if (!at) return
    const next = complete(text, caret, at, handle)
    pendingCaret.current = next.caret
    edit(next.text, next.caret)
    input.current?.focus()
  }

  const send = async () => {
    const value = text.trim()
    if (!value || sending) return
    setSending(true)
    setError(null)
    try {
      await api.post(channelId, value)
      edit('', 0)
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      setSending(false)
    }
  }

  const onKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.nativeEvent.isComposing) return
    if (options.length > 0) {
      if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
        e.preventDefault()
        const step = e.key === 'ArrowDown' ? 1 : -1
        setHighlight((active + step + options.length) % options.length)
        return
      }
      if (e.key === 'Enter' || e.key === 'Tab') {
        e.preventDefault()
        choose(options[active])
        return
      }
      if (e.key === 'Escape') {
        e.preventDefault()
        setDismissed(true)
        return
      }
    }
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault()
      void send()
    }
  }

  const syncCaret = () => setCaret(input.current?.selectionStart ?? text.length)

  return (
    <form
      className="composer"
      onSubmit={(e) => {
        e.preventDefault()
        void send()
      }}
    >
      {error && <p className="error small">{error}</p>}
      <div className="composer-input">
        {options.length > 0 && (
          <ul className="mentions" role="listbox" aria-label="Mention suggestions">
            {options.map((handle, i) => {
              const member = members.find((m) => m.handle === handle)
              return (
                <li
                  key={handle}
                  role="option"
                  aria-selected={i === active}
                  className={i === active ? 'active' : undefined}
                  onMouseDown={(e) => {
                    e.preventDefault()
                    choose(handle)
                  }}
                >
                  <span className="mention">@{handle}</span>
                  {member && <span className="muted small">{member.name}</span>}
                </li>
              )
            })}
          </ul>
        )}
        <textarea
          ref={input}
          value={text}
          onChange={(e) => edit(e.target.value, e.target.selectionStart)}
          onKeyDown={onKeyDown}
          onKeyUp={(e) => {
            if (e.key === 'ArrowLeft' || e.key === 'ArrowRight' || e.key === 'Home' || e.key === 'End') {
              syncCaret()
            }
          }}
          onClick={syncCaret}
          placeholder="Message the channel (@ to mention, Enter to send)"
          rows={3}
          aria-label="Message"
        />
      </div>
      <button type="submit" disabled={sending || !text.trim()}>
        Send
      </button>
    </form>
  )
}
