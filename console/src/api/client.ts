// Typed client for the subspace-server API. Wire types come from
// `./generated` (ts-rs); regenerate them from Rust, don't edit them by hand.

import type { Channel } from './generated/Channel'
import type { ChannelDetail } from './generated/ChannelDetail'
import type { ChannelEvent } from './generated/ChannelEvent'
import type { ChannelPost } from './generated/ChannelPost'
import type { ChannelStreamMessage } from './generated/ChannelStreamMessage'
import type { CreateChannel } from './generated/CreateChannel'
import type { CreateWorker } from './generated/CreateWorker'
import type { ErrorBody } from './generated/ErrorBody'
import type { Health } from './generated/Health'
import type { History } from './generated/History'
import type { ServerEvent } from './generated/ServerEvent'
import type { StreamMessage } from './generated/StreamMessage'
import type { UpdateChannel } from './generated/UpdateChannel'
import type { UpdateWorker } from './generated/UpdateWorker'
import type { WorkerDetail } from './generated/WorkerDetail'
import type { WorkerSummary } from './generated/WorkerSummary'
import type { Workspace } from './generated/Workspace'
import type { WorkspaceDetail } from './generated/WorkspaceDetail'

/** A non-2xx API response, carrying the server's `{"error": ...}` message. */
export class ApiRequestError extends Error {
  readonly status: number

  constructor(status: number, message: string) {
    super(message)
    this.name = 'ApiRequestError'
    this.status = status
  }
}

async function request<T>(method: string, path: string, body?: unknown): Promise<T> {
  const res = await fetch(path, {
    method,
    headers: body === undefined ? undefined : { 'content-type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body),
  })
  if (!res.ok) {
    let message = `${method} ${path} failed with ${res.status}`
    try {
      message = ((await res.json()) as ErrorBody).error
    } catch {
      // Not JSON (e.g. the dev proxy can't reach the server); keep the default.
    }
    throw new ApiRequestError(res.status, message)
  }
  if (res.status === 202 || res.status === 204) {
    return undefined as T
  }
  return (await res.json()) as T
}

const worker = (id: string) => `/api/workers/${encodeURIComponent(id)}`
const workspace = (id: string) => `/api/workspaces/${encodeURIComponent(id)}`
const channel = (id: string) => `/api/channels/${encodeURIComponent(id)}`

export const api = {
  health: () => request<Health>('GET', '/api/health'),
  listWorkers: () => request<WorkerSummary[]>('GET', '/api/workers'),
  getWorker: (id: string) => request<WorkerDetail>('GET', worker(id)),
  createWorker: (body: CreateWorker) => request<WorkerDetail>('POST', '/api/workers', body),
  updateWorker: (id: string, body: UpdateWorker) =>
    request<WorkerDetail>('PATCH', worker(id), body),
  deleteWorker: (id: string) => request<void>('DELETE', worker(id)),
  prompt: (id: string, text: string) => request<void>('POST', `${worker(id)}/prompt`, { text }),
  history: (id: string) => request<History>('GET', `${worker(id)}/history`),

  listWorkspaces: () => request<Workspace[]>('GET', '/api/workspaces'),
  getWorkspace: (id: string) => request<WorkspaceDetail>('GET', workspace(id)),
  createChannel: (workspaceId: string, body: CreateChannel) =>
    request<Channel>('POST', `${workspace(workspaceId)}/channels`, body),
  getChannel: (id: string) => request<ChannelDetail>('GET', channel(id)),
  updateChannel: (id: string, body: UpdateChannel) => request<Channel>('PATCH', channel(id), body),
  deleteChannel: (id: string) => request<void>('DELETE', channel(id)),
  addChannelMember: (id: string, workerId: string) =>
    request<void>('POST', `${channel(id)}/members`, { worker_id: workerId }),
  removeChannelMember: (id: string, workerId: string) =>
    request<void>('DELETE', `${channel(id)}/members/${encodeURIComponent(workerId)}`),
  post: (id: string, text: string) => request<ChannelPost>('POST', `${channel(id)}/posts`, { text }),
}

export interface StreamHandlers<E> {
  /** Every event, in `seq` order, replayed ones first. */
  onEvent: (event: E) => void
  /**
   * The stream can't be caught up (server restarted, too far behind).
   * Refetch history and open a new stream from its cursor.
   */
  onResync: (reason: string) => void
  /** Connection state changes, for showing a "reconnecting" indicator. */
  onConnectionChange?: (connected: boolean) => void
}

/** Where to resume a stream: from `api.history` or `api.getChannel`. */
export interface StreamStart {
  cursor: number
  epoch: string
}

type Frame<E> = { type: 'hello' } | { type: 'event'; event: E } | { type: 'resync'; reason: string }

const RECONNECT_MIN_MS = 500
const RECONNECT_MAX_MS = 10_000

/**
 * Open a worker's event stream starting after `from.cursor`.
 * Unexpected disconnects reconnect automatically from the last seen `seq`,
 * with backoff; a `resync` frame stops the stream and calls `onResync`.
 * Returns a function that closes the stream for good.
 */
export function openWorkerStream(
  id: string,
  from: StreamStart,
  handlers: StreamHandlers<ServerEvent>,
): () => void {
  return openStream<ServerEvent, StreamMessage>(`${worker(id)}/stream`, from, handlers)
}

/** Open a channel's event stream (same behavior as `openWorkerStream`). */
export function openChannelStream(
  id: string,
  from: StreamStart,
  handlers: StreamHandlers<ChannelEvent>,
): () => void {
  return openStream<ChannelEvent, ChannelStreamMessage>(`${channel(id)}/stream`, from, handlers)
}

function openStream<E extends { seq: number }, F extends Frame<E>>(
  path: string,
  from: StreamStart,
  handlers: StreamHandlers<E>,
): () => void {
  let since = from.cursor
  const epoch = from.epoch
  let socket: WebSocket | null = null
  let closed = false
  let retryMs = RECONNECT_MIN_MS
  let retryTimer: ReturnType<typeof setTimeout> | undefined

  const connect = () => {
    const proto = location.protocol === 'https:' ? 'wss:' : 'ws:'
    const query = new URLSearchParams({ since: String(since), epoch })
    const ws = new WebSocket(`${proto}//${location.host}${path}?${query}`)
    socket = ws

    ws.onmessage = (msg) => {
      const frame = JSON.parse(msg.data as string) as F
      switch (frame.type) {
        case 'hello':
          retryMs = RECONNECT_MIN_MS
          handlers.onConnectionChange?.(true)
          break
        case 'event':
          // The server never repeats a seq, but a reconnect race could.
          if (frame.event.seq > since) {
            since = frame.event.seq
            handlers.onEvent(frame.event)
          }
          break
        case 'resync':
          closed = true
          ws.close()
          handlers.onResync(frame.reason)
          break
      }
    }

    ws.onclose = () => {
      if (socket !== ws) return
      socket = null
      handlers.onConnectionChange?.(false)
      if (closed) return
      retryTimer = setTimeout(connect, retryMs)
      retryMs = Math.min(retryMs * 2, RECONNECT_MAX_MS)
    }
  }

  connect()
  return () => {
    closed = true
    clearTimeout(retryTimer)
    socket?.close()
    socket = null
  }
}
