// Pure state for one channel: fetched detail merged with streamed events.
//
// Posts and notices merge by id, so detail and replayed events never
// duplicate. Each worker taking a turn gets its own `ChatState`, driven by
// the same reducer as a DM, and is dropped when it goes idle (its reply
// arrives as a post).

import type { Channel } from '../api/generated/Channel'
import type { ChannelDetail } from '../api/generated/ChannelDetail'
import type { ChannelEvent } from '../api/generated/ChannelEvent'
import type { ChannelMember } from '../api/generated/ChannelMember'
import type { ChannelPost } from '../api/generated/ChannelPost'
import type { Notice } from '../api/generated/Notice'
import { applyWorkerEvent, emptyChat, mergeById, time, type ChatState } from './state.ts'

export interface ChannelState {
  channel: Channel | null
  members: ChannelMember[]
  posts: ChannelPost[]
  notices: Notice[]
  /** Workers mid-turn, by id: their live activity. */
  running: Record<string, ChatState>
  /** Name, topic, or members changed; refetch the detail. */
  stale: boolean
  deleted: boolean
}

export const emptyChannel: ChannelState = {
  channel: null,
  members: [],
  posts: [],
  notices: [],
  running: {},
  stale: false,
  deleted: false,
}

/** Start from fetched detail; live activity is rebuilt by the replay. */
export function fromDetail(detail: ChannelDetail): ChannelState {
  return {
    ...emptyChannel,
    channel: detail.channel,
    members: detail.members,
    posts: mergeById([], detail.posts),
    notices: mergeById([], detail.notices),
  }
}

/** Refresh name, topic, and members without touching the transcript. */
export function withMeta(state: ChannelState, detail: ChannelDetail): ChannelState {
  return { ...state, channel: detail.channel, members: detail.members, stale: false }
}

export function applyChannelEvent(state: ChannelState, event: ChannelEvent): ChannelState {
  const inner = event.inner
  switch (inner.type) {
    case 'post':
      return { ...state, posts: mergeById(state.posts, [inner.data]) }
    case 'notice':
      return { ...state, notices: mergeById(state.notices, [inner.data]) }
    case 'routed': {
      const { post_id, routing } = inner.data
      return {
        ...state,
        posts: state.posts.map((p) => (p.id === post_id ? { ...p, routing } : p)),
      }
    }
    case 'changed':
      return { ...state, stale: true }
    case 'deleted':
      return { ...state, deleted: true, running: {} }
    case 'worker': {
      const { worker_id: id, event: ev } = inner.data
      if (ev.type === 'status_changed' && ev.data.state !== 'running') {
        const running = { ...state.running }
        delete running[id]
        return { ...state, running }
      }
      const current = state.running[id] ?? emptyChat
      return { ...state, running: { ...state.running, [id]: applyWorkerEvent(current, ev) } }
    }
  }
}

export type ChannelItem = { kind: 'post'; post: ChannelPost } | { kind: 'notice'; notice: Notice }

/** Posts in order, with each notice placed by time. */
export function channelTimeline(state: ChannelState): ChannelItem[] {
  const notices = [...state.notices].sort((a, b) => time(a.timestamp) - time(b.timestamp))
  const out: ChannelItem[] = []
  let n = 0
  for (const post of state.posts) {
    while (n < notices.length && time(notices[n].timestamp) < time(post.timestamp)) {
      out.push({ kind: 'notice', notice: notices[n++] })
    }
    out.push({ kind: 'post', post })
  }
  while (n < notices.length) out.push({ kind: 'notice', notice: notices[n++] })
  return out
}
