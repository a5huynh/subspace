import assert from 'node:assert/strict'
import { test } from 'node:test'

import type { ChannelDetail } from '../api/generated/ChannelDetail.ts'
import type { ChannelEvent } from '../api/generated/ChannelEvent.ts'
import type { ChannelEventKind } from '../api/generated/ChannelEventKind.ts'
import type { ChannelPost } from '../api/generated/ChannelPost.ts'
import type { WorkerEvent } from '../api/generated/WorkerEvent.ts'
import { applyChannelEvent, channelTimeline, emptyChannel, fromDetail, withMeta } from './channel.ts'
import { complete, mentionAt, segments, suggest } from './mentions.ts'

let seq = 0
const ev = (inner: ChannelEventKind): ChannelEvent => ({ channel_id: 'c1', seq: ++seq, ts: '', inner })
const worker = (id: string, event: WorkerEvent) => ev({ type: 'worker', data: { worker_id: id, event } })
const run = (events: ChannelEvent[]) => events.reduce(applyChannelEvent, emptyChannel)

const post = (id: string, text: string, timestamp = '2026-01-01T00:00:01Z'): ChannelPost => ({
  id,
  channel_id: 'c1',
  seq: 0,
  author: { kind: 'operator' },
  author_name: 'You',
  text,
  mentions: [],
  root_id: id,
  hops: 0,
  routing: { state: 'pending' },
  timestamp,
})

const detail = (posts: ChannelPost[]): ChannelDetail => ({
  epoch: 'e',
  cursor: 0,
  channel: { id: 'c1', workspace_id: 'w', name: 'launch', topic: '', created_at: '' },
  members: [],
  posts,
  notices: [],
})

test('posts merge by id across detail and replay', () => {
  const p = post('p1', 'hi')
  const s = [ev({ type: 'post', data: p }), ev({ type: 'post', data: p })].reduce(
    applyChannelEvent,
    fromDetail(detail([p])),
  )
  assert.deepEqual(s.posts, [p])
})

test('a routing decision updates its post, even after a replay', () => {
  const p = post('p1', 'who knows?')
  const routed = ev({
    type: 'routed',
    data: { post_id: 'p1', routing: { state: 'chosen', worker_id: 'w1', reason: 'facts' } },
  })
  const s = [ev({ type: 'post', data: p }), routed].reduce(applyChannelEvent, fromDetail(detail([p])))
  assert.deepEqual(s.posts[0].routing, { state: 'chosen', worker_id: 'w1', reason: 'facts' })
})

test('a worker turn streams live, then clears when it goes idle', () => {
  let s = run([
    worker('w1', { type: 'status_changed', data: { state: 'running' } }),
    worker('w1', { type: 'agent', data: { type: 'agent_start' } }),
    worker('w1', { type: 'agent', data: { type: 'text_delta', data: 'Draft' } }),
    worker('w2', { type: 'status_changed', data: { state: 'running' } }),
  ])
  assert.deepEqual(Object.keys(s.running).sort(), ['w1', 'w2'])
  assert.deepEqual(s.running.w1.live, [{ kind: 'text', text: 'Draft' }])

  s = [worker('w1', { type: 'status_changed', data: { state: 'idle' } })].reduce(applyChannelEvent, s)
  assert.deepEqual(Object.keys(s.running), ['w2'])
})

test('changes mark the detail stale; deletion stops everything', () => {
  let s = run([ev({ type: 'changed' })])
  assert.equal(s.stale, true)
  s = withMeta(s, detail([]))
  assert.equal(s.stale, false)

  s = run([worker('w1', { type: 'status_changed', data: { state: 'running' } }), ev({ type: 'deleted' })])
  assert.equal(s.deleted, true)
  assert.deepEqual(s.running, {})
})

test('notices are placed among posts by time', () => {
  let s = fromDetail(detail([post('p1', 'a', '2026-01-01T00:00:01Z'), post('p2', 'b', '2026-01-01T00:00:05Z')]))
  s = applyChannelEvent(
    s,
    ev({ type: 'notice', data: { id: 'n1', level: 'warning', text: 'stopped', timestamp: '2026-01-01T00:00:03Z' } }),
  )
  const order = channelTimeline(s).map((i) => (i.kind === 'post' ? i.post.id : i.notice.id))
  assert.deepEqual(order, ['p1', 'n1', 'p2'])
})

test('mentionAt finds the mention being typed, not emails', () => {
  assert.deepEqual(mentionAt('hi @wri', 7), { start: 3, query: 'wri' })
  assert.deepEqual(mentionAt('@', 1), { start: 0, query: '' })
  assert.equal(mentionAt('mail a@b', 8), null)
  assert.equal(mentionAt('@writer done', 12), null)
})

test('suggest prefers prefix matches; complete inserts the handle', () => {
  assert.deepEqual(suggest(['writer', 'rewriter', 'researcher'], 'wri'), ['writer', 'rewriter'])
  const at = mentionAt('ask @wr please', 7)!
  assert.deepEqual(complete('ask @wr please', 7, at, 'writer'), { text: 'ask @writer please', caret: 12 })
  const end = mentionAt('ask @wr', 7)!
  assert.deepEqual(complete('ask @wr', 7, end, 'writer'), { text: 'ask @writer ', caret: 12 })
})

test('segments highlight only known handles', () => {
  assert.deepEqual(segments('@writer, ping @ghost and a@b.com', new Set(['writer'])), [
    { kind: 'mention', text: '@writer', handle: 'writer' },
    { kind: 'text', text: ', ping @ghost and a@b.com' },
  ])
})
