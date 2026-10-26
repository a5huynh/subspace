// Pure state for one workspace: fetched detail merged with streamed events.
//
// Posts and notices merge by id, so detail and replayed events never
// duplicate. Each worker taking a turn gets its own `ChatState`, driven by
// the same reducer as a DM, and is dropped when it goes idle (its reply
// arrives as a post).

import type { Workspace } from '../api/generated/Workspace'
import type { WorkspaceDetail } from '../api/generated/WorkspaceDetail'
import type { WorkspaceEvent } from '../api/generated/WorkspaceEvent'
import type { WorkspaceMember } from '../api/generated/WorkspaceMember'
import type { WorkspacePost } from '../api/generated/WorkspacePost'
import type { Notice } from '../api/generated/Notice'
import { applyWorkerEvent, emptyChat, mergeById, time, type ChatState } from './state.ts'

export interface WorkspaceState {
  workspace: Workspace | null
  members: WorkspaceMember[]
  posts: WorkspacePost[]
  notices: Notice[]
  /** Workers mid-turn, by id: their live activity. */
  running: Record<string, ChatState>
  /** Name, topic, or members changed; refetch the detail. */
  stale: boolean
  deleted: boolean
}

export const emptyWorkspace: WorkspaceState = {
  workspace: null,
  members: [],
  posts: [],
  notices: [],
  running: {},
  stale: false,
  deleted: false,
}

/** Start from fetched detail; live activity is rebuilt by the replay. */
export function fromDetail(detail: WorkspaceDetail): WorkspaceState {
  return {
    ...emptyWorkspace,
    workspace: detail.workspace,
    members: detail.members,
    posts: mergeById([], detail.posts),
    notices: mergeById([], detail.notices),
  }
}

/** Refresh name, topic, and members without touching the transcript. */
export function withMeta(state: WorkspaceState, detail: WorkspaceDetail): WorkspaceState {
  return { ...state, workspace: detail.workspace, members: detail.members, stale: false }
}

export function applyWorkspaceEvent(state: WorkspaceState, event: WorkspaceEvent): WorkspaceState {
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

export type WorkspaceItem = { kind: 'post'; post: WorkspacePost } | { kind: 'notice'; notice: Notice }

/** Posts in order, with each notice placed by time. */
export function workspaceTimeline(state: WorkspaceState): WorkspaceItem[] {
  const notices = [...state.notices].sort((a, b) => time(a.timestamp) - time(b.timestamp))
  const out: WorkspaceItem[] = []
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
