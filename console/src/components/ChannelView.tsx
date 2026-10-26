import { useMemo, type ReactNode } from 'react'

import { api } from '../api/client'
import type { ChannelMember } from '../api/generated/ChannelMember'
import type { Routing } from '../api/generated/Routing'
import { channelTimeline } from '../chat/channel'
import { layoutRows } from '../chat/format'
import { OPERATOR_HANDLE, segments } from '../chat/mentions'
import { isToolResultMessage, toolResults, type ChatState } from '../chat/state'
import { useChannel } from '../chat/useChannel'
import { href } from '../route'
import { useNow } from '../useNow'
import { useScrollToBottom } from '../useScrollToBottom'
import { LiveContent, MessageContent } from './MessageView'
import {
  Avatar,
  Composer,
  ConnectionDot,
  ConversationHeader,
  DayDivider,
  EmptyState,
  MessageList,
  MessageRow,
  NoticeRow,
} from './ui'

export function ChannelView({ id }: { id: string }) {
  const { state, connected, error } = useChannel(id)
  const now = useNow()
  const scroller = useScrollToBottom([state.posts, state.notices, state.running])
  const known = useMemo(
    () => new Set([OPERATOR_HANDLE, ...state.members.map((m) => m.handle)]),
    [state.members],
  )

  if (state.deleted) {
    return (
      <EmptyState>
        This channel was deleted. <a href={href({ page: 'home' })}>Back home</a>
      </EmptyState>
    )
  }

  const channel = state.channel
  const nameOf = (workerId: string) =>
    state.members.find((m) => m.worker_id === workerId)?.name ?? 'A worker'
  const renderText = (text: string) => <PostText text={text} known={known} />

  const items = channelTimeline(state)
  const running = Object.entries(state.running)
  const layout = layoutRows([
    ...items.map((item) =>
      item.kind === 'post'
        ? {
            author:
              item.post.author.kind === 'operator' ? 'operator' : `worker:${item.post.author.id}`,
            timestamp: item.post.timestamp,
          }
        : { author: null, timestamp: item.notice.timestamp },
    ),
    // Live turns always get their own header.
    ...running.map(() => ({ author: null, timestamp: now })),
  ])

  return (
    <section className="flex min-h-0 flex-1 flex-col">
      <ConversationHeader
        title={
          <>
            <span className="font-normal text-muted">#</span>
            <span className="truncate">{channel?.name ?? '…'}</span>
          </>
        }
        subtitle={channel?.topic || undefined}
        right={
          <>
            <ConnectionDot connected={connected} />
            <MemberStack members={state.members} channelId={id} />
          </>
        }
      />

      <MessageList scroller={scroller}>
        {error && <p className="px-5 text-[13px] text-danger">{error}</p>}
        {channel && items.length === 0 && running.length === 0 && (
          <EmptyState>
            <span className="mb-1 block text-lg font-black text-ink">Welcome to #{channel.name}</span>
            {state.members.length === 0
              ? 'Add workers in Settings to get them talking.'
              : 'Ask anything: the best-suited member answers, or @mention someone directly.'}
          </EmptyState>
        )}
        {items.map((item, i) => {
          const { dayBreak, continued } = layout[i]
          if (item.kind === 'notice') {
            return (
              <div key={item.notice.id}>
                {dayBreak && <DayDivider timestamp={item.notice.timestamp} />}
                <NoticeRow notice={item.notice} />
              </div>
            )
          }
          const post = item.post
          return (
            <div key={post.id}>
              {dayBreak && <DayDivider timestamp={post.timestamp} />}
              <MessageRow name={post.author_name} timestamp={post.timestamp} continued={continued}>
                <div>{renderText(post.text)}</div>
                <RoutingNote routing={post.routing} nameOf={nameOf} />
              </MessageRow>
            </div>
          )
        })}
        {running.map(([workerId, chat], i) => (
          <div key={workerId}>
            {layout[items.length + i].dayBreak && <DayDivider timestamp={now} />}
            <MessageRow name={nameOf(workerId)} timestamp={now} continued={false} live>
              <Working chat={chat} renderText={renderText} />
            </MessageRow>
          </div>
        ))}
      </MessageList>

      <Composer
        placeholder={`Message #${channel?.name ?? ''}`}
        members={state.members}
        onSend={async (text) => {
          await api.post(id, text)
        }}
      />
    </section>
  )
}

/** Overlapping member avatars that open the channel's settings. */
function MemberStack({ members, channelId }: { members: ChannelMember[]; channelId: string }) {
  const shown = members.slice(0, 3)
  return (
    <a
      href={href({ page: 'channel-settings', id: channelId })}
      className="btn-secondary gap-2 py-1 pl-1.5"
      title={['You', ...members.map((m) => `${m.name} (@${m.handle})`)].join('\n')}
    >
      <span className="flex -space-x-1">
        <span className="rounded ring-2 ring-page">
          <Avatar name="You" size="xs" />
        </span>
        {shown.map((m) => (
          <span key={m.worker_id} className="rounded ring-2 ring-page">
            <Avatar name={m.name} size="xs" />
          </span>
        ))}
      </span>
      {members.length + 1}
    </a>
  )
}

/** Post text with known @mentions as chips. */
function PostText({ text, known }: { text: string; known: Set<string> }): ReactNode {
  return (
    <span className="break-words whitespace-pre-wrap">
      {segments(text, known).map((s, i) =>
        s.kind === 'mention' ? (
          <span
            key={i}
            className={`rounded px-0.5 font-medium ${
              s.handle === OPERATOR_HANDLE ? 'bg-mention-me text-ink' : 'bg-mention text-mention-ink'
            }`}
          >
            {s.text}
          </span>
        ) : (
          <span key={i}>{s.text}</span>
        ),
      )}
    </span>
  )
}

/** Who an unaddressed post was given to, and why. */
function RoutingNote({ routing, nameOf }: { routing: Routing; nameOf: (workerId: string) => string }) {
  const note = (children: ReactNode, extra = '') => (
    <p className={`flex items-center gap-1 text-xs text-muted ${extra}`}>{children}</p>
  )
  switch (routing.state) {
    case 'direct':
    case 'failed':
      // Failures are explained by a channel notice.
      return null
    case 'pending':
      return note('Choosing who should answer…', 'animate-pulse')
    case 'chosen':
      return note(
        <>
          <span aria-hidden>↳</span> Routed to <b className="font-bold text-ink">{nameOf(routing.worker_id)}</b>
          {routing.reason && <span>· {routing.reason}</span>}
        </>,
      )
    case 'nobody':
      return note(<>No reply needed{routing.reason && <span>· {routing.reason}</span>}</>)
  }
}

/** A worker mid-turn: its finished steps (tool calls) and live output. */
function Working({ chat, renderText }: { chat: ChatState; renderText: (text: string) => ReactNode }) {
  const results = toolResults(chat)
  return (
    <>
      {chat.messages
        .filter((m) => m.role === 'assistant' && !isToolResultMessage(m))
        .map((m) => (
          <MessageContent key={m.id} message={m} results={results} renderText={renderText} />
        ))}
      {chat.live && <LiveContent blocks={chat.live} results={results} renderText={renderText} />}
    </>
  )
}
