import { api } from '../api/client'
import type { WorkerSummary } from '../api/generated/WorkerSummary'
import { layoutRows } from '../chat/format'
import { timeline, toolResults } from '../chat/state'
import { useWorkerChat } from '../chat/useWorkerChat'
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

/** A direct message with one worker. */
export function Chat({ worker }: { worker: WorkerSummary }) {
  const { state, connected, error } = useWorkerChat(worker.id)
  const status = state.status ?? worker.status
  const results = toolResults(state)
  const now = useNow()
  const scroller = useScrollToBottom([state.messages, state.live, state.notices])

  const items = timeline(state)
  const authors = [
    ...items.map((item) => ({
      author: item.kind === 'message' ? item.message.role : null,
      timestamp: item.kind === 'message' ? item.message.timestamp : item.notice.timestamp,
    })),
    ...(state.live ? [{ author: 'assistant', timestamp: now }] : []),
  ]
  const layout = layoutRows(authors)

  return (
    <section className="flex min-h-0 flex-1 flex-col">
      <ConversationHeader
        title={
          <>
            <Avatar name={worker.name} size="sm" status={status} ring="ring-page" />
            <span className="truncate">{worker.name}</span>
          </>
        }
        subtitle={worker.description || undefined}
        right={
          <>
            <ConnectionDot connected={connected} />
            <a className="btn-secondary" href={href({ page: 'settings', id: worker.id })}>
              Settings
            </a>
          </>
        }
      />

      <MessageList scroller={scroller}>
        {error && <p className="px-5 text-[13px] text-danger">{error}</p>}
        {items.length === 0 && !state.live && (
          <EmptyState>
            This is the very beginning of your direct message history with <b>{worker.name}</b>.
          </EmptyState>
        )}
        {items.map((item, i) => {
          const { dayBreak, continued } = layout[i]
          const timestamp = item.kind === 'message' ? item.message.timestamp : item.notice.timestamp
          return (
            <div key={item.kind === 'message' ? item.message.id : item.notice.id}>
              {dayBreak && <DayDivider timestamp={timestamp} />}
              {item.kind === 'message' ? (
                <MessageRow
                  name={item.message.role === 'user' ? 'You' : worker.name}
                  timestamp={timestamp}
                  continued={continued}
                >
                  <MessageContent message={item.message} results={results} />
                </MessageRow>
              ) : (
                <NoticeRow notice={item.notice} />
              )}
            </div>
          )
        })}
        {state.live && (
          <>
            {layout[items.length].dayBreak && <DayDivider timestamp={now} />}
            <MessageRow name={worker.name} timestamp={now} continued={layout[items.length].continued} live>
              <LiveContent blocks={state.live} results={results} />
            </MessageRow>
          </>
        )}
      </MessageList>

      <Composer placeholder={`Message ${worker.name}`} onSend={(text) => api.prompt(worker.id, text)} />
    </section>
  )
}
