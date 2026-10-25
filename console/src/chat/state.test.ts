// Run with `npm test` (Node's built-in runner; Node strips the types).

import assert from 'node:assert/strict'
import { test } from 'node:test'

import type { AgentEvent } from '../api/generated/AgentEvent.ts'
import type { Message } from '../api/generated/Message.ts'
import type { ServerEvent } from '../api/generated/ServerEvent.ts'
import type { WorkerEvent } from '../api/generated/WorkerEvent.ts'
import {
  applyEvent,
  emptyChat,
  fromHistory,
  isToolResultMessage,
  toolResults,
  type ChatState,
} from './state.ts'

let seq = 0
const ev = (inner: WorkerEvent): ServerEvent => ({ worker_id: 'w', seq: ++seq, ts: '', inner })
const agent = (data: AgentEvent) => ev({ type: 'agent', data })
const msg = (id: string, role: 'user' | 'assistant', text: string): Message => ({
  id,
  role,
  content: [{ type: 'text', text }],
  timestamp: '',
})
const usage = { input_tokens: 0, output_tokens: 0, cache_read_tokens: 0, cache_write_tokens: 0 }
const run = (state: ChatState, events: ServerEvent[]) => events.reduce(applyEvent, state)

test('streams text into a live block, then commits the message on turn_end', () => {
  const reply = msg('a1', 'assistant', 'Hello there')
  let s = run(emptyChat, [
    ev({ type: 'user_message', data: msg('u1', 'user', 'hi') }),
    agent({ type: 'agent_start' }),
    agent({ type: 'turn_start', data: { turn: 1 } }),
    agent({ type: 'text_delta', data: 'Hello ' }),
    agent({ type: 'text_delta', data: 'there' }),
  ])
  assert.deepEqual(s.live, [{ kind: 'text', text: 'Hello there' }])
  assert.deepEqual(s.messages.map((m) => m.id), ['u1'])

  s = run(s, [agent({ type: 'turn_end', data: { turn: 1, message: reply, usage } })])
  assert.deepEqual(s.live, [])
  s = run(s, [agent({ type: 'agent_end', data: { messages: [reply], total_usage: usage } })])
  assert.equal(s.live, null)
  assert.deepEqual(s.messages.map((m) => m.id), ['u1', 'a1'], 'agent_end does not duplicate')
})

test('replaying a run over history does not duplicate messages', () => {
  const user = msg('u1', 'user', 'hi')
  const reply = msg('a1', 'assistant', 'yo')
  const history = fromHistory(emptyChat, [user, reply])
  const s = run(history, [
    ev({ type: 'status_changed', data: { state: 'running' } }),
    ev({ type: 'user_message', data: user }),
    agent({ type: 'agent_start' }),
    agent({ type: 'text_delta', data: 'yo' }),
    agent({ type: 'turn_end', data: { turn: 1, message: reply, usage } }),
    agent({ type: 'agent_end', data: { messages: [reply], total_usage: usage } }),
    ev({ type: 'status_changed', data: { state: 'idle' } }),
  ])
  assert.deepEqual(s.messages.map((m) => m.id), ['u1', 'a1'])
  assert.deepEqual(s.status, { state: 'idle' })
})

test('tool calls keep their live result until the tool-result message arrives', () => {
  const call: Message = {
    id: 'a1',
    role: 'assistant',
    content: [{ type: 'tool_use', id: 't1', name: 'home_read', input: { path: 'x' } }],
    timestamp: '',
  }
  const result = { content: [{ type: 'text' as const, text: 'file body' }], is_error: false }
  let s = run(emptyChat, [
    agent({ type: 'agent_start' }),
    agent({ type: 'tool_call_start', data: { id: 't1', name: 'home_read', arguments: { path: 'x' } } }),
    agent({ type: 'tool_exec_update', data: { id: 't1', name: 'home_read', partial: 'fi' } }),
  ])
  assert.deepEqual(s.live, [
    { kind: 'tool', id: 't1', name: 'home_read', args: { path: 'x' }, partial: 'fi' },
  ])

  s = run(s, [
    agent({ type: 'tool_exec_end', data: { id: 't1', name: 'home_read', result } }),
    agent({ type: 'turn_end', data: { turn: 1, message: call, usage } }),
  ])
  assert.deepEqual(toolResults(s).t1, result, 'live result available between turn_end and agent_end')

  const resultMsg: Message = {
    id: 'r1',
    role: 'user',
    content: [{ type: 'tool_result', tool_use_id: 't1', content: result.content, is_error: false }],
    timestamp: '',
  }
  s = run(s, [agent({ type: 'agent_end', data: { messages: [call, resultMsg], total_usage: usage } })])
  assert.deepEqual(s.liveResults, {})
  assert.deepEqual(toolResults(s).t1, result, 'now from the persisted message')
  assert.ok(isToolResultMessage(resultMsg))
  assert.ok(!isToolResultMessage(call))
})

test('thinking and text become separate blocks; errors become notices', () => {
  const s = run(emptyChat, [
    agent({ type: 'agent_start' }),
    agent({ type: 'thinking_delta', data: 'hmm ' }),
    agent({ type: 'thinking_delta', data: 'ok' }),
    agent({ type: 'text_delta', data: 'Answer' }),
    agent({ type: 'error', data: 'rate limited' }),
  ])
  assert.deepEqual(s.live, [
    { kind: 'thinking', text: 'hmm ok' },
    { kind: 'text', text: 'Answer' },
  ])
  assert.equal(s.notices.length, 1)
  assert.equal(s.notices[0].level, 'error')
  assert.equal(s.notices[0].text, 'rate limited')
})

test('fromHistory keeps the status but resets live state', () => {
  const s = fromHistory(
    { ...emptyChat, status: { state: 'running' }, live: [{ kind: 'text', text: 'partial' }] },
    [msg('u1', 'user', 'hi')],
  )
  assert.equal(s.live, null)
  assert.deepEqual(s.status, { state: 'running' })
  assert.equal(s.messages.length, 1)
})
