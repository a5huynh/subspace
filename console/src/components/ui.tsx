import { useLayoutEffect, useRef, useState, type KeyboardEvent, type ReactNode } from 'react'

import type { ChannelMember } from '../api/generated/ChannelMember'
import type { Notice } from '../api/generated/Notice'
import type { WorkerStatus } from '../api/generated/WorkerStatus'
import { avatarColor, dayLabel, formatFull, formatTime, initials, summarizeNotice } from '../chat/format'
import { complete, mentionAt, suggest } from '../chat/mentions'

const STATUS_DOT: Record<WorkerStatus['state'], string> = {
  idle: 'bg-online',
  running: 'bg-online animate-pulse',
  paused: 'bg-warning',
  error: 'bg-danger',
}

export function Avatar({
  name,
  size = 'md',
  status,
  ring = 'ring-sidebar',
}: {
  name: string
  size?: 'xs' | 'sm' | 'md'
  status?: WorkerStatus
  /** Ring around the status dot: match the background it sits on. */
  ring?: string
}) {
  const box = { xs: 'size-5 text-[9px] rounded', sm: 'size-6 text-[10px] rounded', md: 'size-9 text-sm rounded-md' }[size]
  return (
    <span className="relative inline-flex shrink-0">
      <span
        className={`${box} flex items-center justify-center font-bold text-white select-none`}
        style={{ background: avatarColor(name) }}
        aria-hidden
      >
        {initials(name)}
      </span>
      {status && (
        <span
          className={`absolute rounded-full ${ring} ${STATUS_DOT[status.state]} ${
            size === 'md' ? '-right-0.5 -bottom-0.5 size-2.5 ring-2' : '-right-0.5 -bottom-0.5 size-2 ring-[1.5px]'
          }`}
          aria-label={status.state}
        />
      )}
    </span>
  )
}

/**
 * One message in Slack's layout: avatar, bold name, time, then content.
 * `continued` rows (same author, moments later) drop the avatar and header
 * and show the time in the gutter on hover.
 */
export function MessageRow({
  name,
  timestamp,
  continued,
  live,
  children,
}: {
  name: string
  timestamp: string
  continued: boolean
  live?: boolean
  children: ReactNode
}) {
  return (
    <div
      className={`group flex gap-2 px-5 hover:bg-hover ${continued ? 'py-0.5' : 'pt-2 pb-1'} ${
        live ? 'bg-highlight hover:bg-highlight' : ''
      }`}
    >
      <div className="w-9 shrink-0">
        {continued ? (
          <time
            className="hidden pt-0.5 text-right text-[11px] leading-[22px] text-muted group-hover:block"
            dateTime={timestamp}
            title={formatFull(timestamp)}
          >
            {formatTime(timestamp).replace(/\s?[AP]M$/i, '')}
          </time>
        ) : (
          <div className="pt-0.5">
            <Avatar name={name} />
          </div>
        )}
      </div>
      <div className="min-w-0 flex-1">
        {!continued && (
          <div className="flex items-baseline gap-2">
            <span className="font-black">{name}</span>
            <time className="text-xs text-muted" dateTime={timestamp} title={formatFull(timestamp)}>
              {formatTime(timestamp)}
            </time>
            {live && <span className="animate-pulse text-xs text-muted">working…</span>}
          </div>
        )}
        <div className="flex flex-col gap-1">{children}</div>
      </div>
    </div>
  )
}

export function DayDivider({ timestamp }: { timestamp: string }) {
  return (
    <div className="relative my-2 flex items-center px-5" role="separator">
      <div className="h-px flex-1 bg-line" />
      <span className="rounded-full border border-line bg-page px-3 py-0.5 text-[13px] font-bold">
        {dayLabel(timestamp)}
      </span>
      <div className="h-px flex-1 bg-line" />
    </div>
  )
}

/** A compact system line: the readable message, with any raw detail tucked away. */
export function NoticeRow({ notice }: { notice: Notice }) {
  const { summary, detail } = summarizeNotice(notice.text)
  const tone = notice.level === 'error' ? 'text-danger' : 'text-warning'
  return (
    <div className={`flex gap-2 px-5 py-1 text-[13px] ${tone}`}>
      <span className="w-9 shrink-0 text-center" aria-hidden>
        {notice.level === 'error' ? '⚠' : 'ⓘ'}
      </span>
      <div className="min-w-0 flex-1">
        <span title={formatFull(notice.timestamp)}>{summary}</span>
        {detail && (
          <details className="mt-0.5 text-muted">
            <summary className="cursor-pointer text-xs">Details</summary>
            <pre className="mt-1 font-mono text-xs break-all whitespace-pre-wrap">{detail}</pre>
          </details>
        )}
      </div>
    </div>
  )
}

/** The scrollable message list. */
export function MessageList({
  scroller,
  children,
}: {
  scroller: React.RefObject<HTMLDivElement | null>
  children: ReactNode
}) {
  return (
    <div ref={scroller} className="flex min-h-0 flex-1 flex-col overflow-y-auto pt-4 pb-2">
      <div className="mt-auto">{children}</div>
    </div>
  )
}

/** Empty-state copy shown in a message list. */
export function EmptyState({ children }: { children: ReactNode }) {
  return <p className="m-auto max-w-md px-5 py-16 text-center text-muted">{children}</p>
}

/** The header bar shared by DMs and channels. */
export function ConversationHeader({
  title,
  subtitle,
  right,
}: {
  title: ReactNode
  subtitle?: ReactNode
  right?: ReactNode
}) {
  return (
    <header className="flex min-h-[57px] items-center justify-between gap-4 border-b border-line px-5 py-2">
      <div className="min-w-0">
        <h1 className="flex items-center gap-1.5 truncate text-lg font-black">{title}</h1>
        {subtitle && <p className="truncate text-[13px] text-muted">{subtitle}</p>}
      </div>
      <div className="flex shrink-0 items-center gap-2">{right}</div>
    </header>
  )
}

/** Shows whether the event stream is connected. */
export function ConnectionDot({ connected }: { connected: boolean }) {
  return (
    <span
      className={`inline-flex items-center gap-1.5 text-xs ${connected ? 'text-muted' : 'text-warning'}`}
      title={connected ? 'Live' : 'Reconnecting'}
    >
      <span className={`size-2 rounded-full ${connected ? 'bg-online' : 'animate-pulse bg-warning'}`} />
      {connected ? 'Live' : 'Connecting…'}
    </span>
  )
}

/**
 * Slack's composer: one rounded box with an auto-growing textarea and the send
 * button inside. Pass `members` to enable @mention autocomplete.
 */
export function Composer({
  placeholder,
  onSend,
  members,
}: {
  placeholder: string
  onSend: (text: string) => Promise<void>
  members?: ChannelMember[]
}) {
  const [text, setText] = useState('')
  const [caret, setCaret] = useState(0)
  const [highlight, setHighlight] = useState(0)
  const [dismissed, setDismissed] = useState(false)
  const [sending, setSending] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const input = useRef<HTMLTextAreaElement>(null)
  const pendingCaret = useRef<number | null>(null)

  const at = members ? mentionAt(text, caret) : null
  const options = at && !dismissed ? suggest((members ?? []).map((m) => m.handle), at.query) : []
  const active = Math.min(highlight, Math.max(options.length - 1, 0))

  useLayoutEffect(() => {
    const el = input.current
    if (!el) return
    el.style.height = 'auto'
    el.style.height = `${Math.min(el.scrollHeight, 240)}px`
    if (pendingCaret.current !== null) {
      el.setSelectionRange(pendingCaret.current, pendingCaret.current)
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
      await onSend(value)
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
      className="px-5 pb-5"
      onSubmit={(e) => {
        e.preventDefault()
        void send()
      }}
    >
      {error && <p className="mb-1 text-[13px] text-danger">{error}</p>}
      <div className="relative rounded-lg border border-line bg-page shadow-xs focus-within:border-muted">
        {options.length > 0 && (
          <ul
            className="absolute bottom-[calc(100%+6px)] left-0 z-10 min-w-64 rounded-lg border border-line bg-page p-1 shadow-lg"
            role="listbox"
            aria-label="Mention suggestions"
          >
            {options.map((handle, i) => {
              const member = members?.find((m) => m.handle === handle)
              return (
                <li
                  key={handle}
                  role="option"
                  aria-selected={i === active}
                  className={`flex cursor-pointer items-center gap-2 rounded-md px-2 py-1.5 ${
                    i === active ? 'bg-sidebar-active text-white' : ''
                  }`}
                  onMouseDown={(e) => {
                    e.preventDefault()
                    choose(handle)
                  }}
                >
                  <Avatar name={member?.name ?? handle} size="sm" />
                  <span className="font-bold">{member?.name ?? handle}</span>
                  <span className={i === active ? 'text-white/80' : 'text-muted'}>@{handle}</span>
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
            if (['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(e.key)) syncCaret()
          }}
          onClick={syncCaret}
          placeholder={placeholder}
          rows={1}
          aria-label="Message"
          className="block max-h-60 w-full resize-none bg-transparent px-3 pt-2.5 pb-1 outline-none placeholder:text-muted"
        />
        <div className="flex items-center justify-between px-2 pb-1.5">
          <span className="text-[11px] text-muted">
            <b>Enter</b> to send · <b>Shift + Enter</b> for a new line
          </span>
          <button
            type="submit"
            disabled={sending || !text.trim()}
            aria-label="Send"
            className="flex size-8 cursor-pointer items-center justify-center rounded-md bg-send text-white transition-colors disabled:cursor-default disabled:bg-transparent disabled:text-muted"
          >
            <svg viewBox="0 0 20 20" className="size-4" fill="currentColor" aria-hidden>
              <path d="M2.4 2.6a.7.7 0 0 1 .8-.1l14.3 6.8a.7.7 0 0 1 0 1.3L3.2 17.4a.7.7 0 0 1-1-.8l1.6-5.9 6.7-.7-6.7-.7-1.6-5.9a.7.7 0 0 1 .2-.8Z" />
            </svg>
          </button>
        </div>
      </div>
    </form>
  )
}

/** A centered page for forms and settings. */
export function Panel({ title, children }: { title: ReactNode; children: ReactNode }) {
  return (
    <section className="min-h-0 flex-1 overflow-y-auto">
      <div className="mx-auto w-full max-w-2xl px-8 py-8">
        <h1 className="mb-6 text-2xl font-black">{title}</h1>
        {children}
      </div>
    </section>
  )
}

