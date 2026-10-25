// Pure chat state for one worker: fetched history merged with streamed events.
//
// Event order within a run (from the engine's agent loop):
//   agent_start, then per turn:
//     turn_start → text/thinking deltas + tool_call_start → tool_exec_* → turn_end{assistant}
//   agent_end{all produced messages, including tool-result messages}
// Messages are merged by id, so history and replayed events never duplicate.

import type { AgentEvent } from '../api/generated/AgentEvent'
import type { Message } from '../api/generated/Message'
import type { ServerEvent } from '../api/generated/ServerEvent'
import type { ToolResult } from '../api/generated/ToolResult'
import type { WorkerStatus } from '../api/generated/WorkerStatus'
import type { JsonValue } from '../api/generated/serde_json/JsonValue'

export type LiveBlock =
  | { kind: 'text'; text: string }
  | { kind: 'thinking'; text: string }
  | { kind: 'tool'; id: string; name: string; args: JsonValue; partial: string }

export interface Notice {
  seq: number
  level: 'warning' | 'error'
  text: string
}

export interface ChatState {
  messages: Message[]
  /** The in-progress assistant turn, or null between runs. */
  live: LiveBlock[] | null
  /** Tool results seen live, until agent_end delivers the tool-result message. */
  liveResults: Record<string, ToolResult>
  /** Errors and warnings aren't persisted, so they only live client-side. */
  notices: Notice[]
  status: WorkerStatus | null
}

export const emptyChat: ChatState = {
  messages: [],
  live: null,
  liveResults: {},
  notices: [],
  status: null,
}

/** Append messages whose ids aren't already present, preserving order. */
export function mergeMessages(existing: Message[], incoming: Message[]): Message[] {
  const seen = new Set(existing.map((m) => m.id))
  const added = incoming.filter((m) => !seen.has(m.id) && (seen.add(m.id), true))
  return added.length === 0 ? existing : [...existing, ...added]
}

/** Start from fetched history. Keeps the known status; clears live state,
 * which a replay from the history cursor rebuilds. */
export function fromHistory(prev: ChatState, messages: Message[]): ChatState {
  return { ...emptyChat, messages: mergeMessages([], messages), status: prev.status }
}

export function applyEvent(state: ChatState, event: ServerEvent): ChatState {
  const inner = event.inner
  switch (inner.type) {
    case 'user_message':
      return { ...state, messages: mergeMessages(state.messages, [inner.data]) }
    case 'status_changed':
      return { ...state, status: inner.data }
    case 'stimulus_received':
      return state
    case 'agent':
      return applyAgentEvent(state, inner.data, event.seq)
  }
}

function applyAgentEvent(state: ChatState, ev: AgentEvent, seq: number): ChatState {
  const live = state.live ?? []
  switch (ev.type) {
    case 'agent_start':
      return { ...state, live: [] }
    case 'turn_start':
      return state.live ? state : { ...state, live: [] }
    case 'text_delta':
      return { ...state, live: appendDelta(live, 'text', ev.data) }
    case 'thinking_delta':
      return { ...state, live: appendDelta(live, 'thinking', ev.data) }
    case 'tool_call_start':
      return {
        ...state,
        live: [
          ...live,
          { kind: 'tool', id: ev.data.id, name: ev.data.name, args: ev.data.arguments, partial: '' },
        ],
      }
    case 'tool_exec_update':
      return {
        ...state,
        live: live.map((b) =>
          b.kind === 'tool' && b.id === ev.data.id ? { ...b, partial: b.partial + ev.data.partial } : b,
        ),
      }
    case 'tool_exec_end':
      return { ...state, liveResults: { ...state.liveResults, [ev.data.id]: ev.data.result } }
    case 'turn_end':
      // The turn's assistant message is final; the next turn streams afresh.
      return { ...state, messages: mergeMessages(state.messages, [ev.data.message]), live: [] }
    case 'agent_end':
      return {
        ...state,
        messages: mergeMessages(state.messages, ev.data.messages),
        live: null,
        liveResults: {},
      }
    case 'warning':
    case 'error':
      return { ...state, notices: [...state.notices, { seq, level: ev.type, text: ev.data }] }
  }
}

function appendDelta(live: LiveBlock[], kind: 'text' | 'thinking', delta: string): LiveBlock[] {
  const last = live[live.length - 1]
  if (last && last.kind === kind) {
    return [...live.slice(0, -1), { kind, text: last.text + delta }]
  }
  return [...live, { kind, text: delta }]
}

/** Tool results by tool_use id, from persisted tool-result messages and live results. */
export function toolResults(state: ChatState): Record<string, ToolResult> {
  const out: Record<string, ToolResult> = { ...state.liveResults }
  for (const m of state.messages) {
    for (const b of m.content) {
      if (b.type === 'tool_result') {
        out[b.tool_use_id] = { content: b.content, is_error: b.is_error }
      }
    }
  }
  return out
}

/** Whether a message is only tool results (rendered inline with its tool call, not as a bubble). */
export function isToolResultMessage(m: Message): boolean {
  return m.content.length > 0 && m.content.every((b) => b.type === 'tool_result')
}
