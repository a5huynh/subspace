import type { ReactNode } from 'react'

import type { ContentBlock } from '../api/generated/ContentBlock'
import type { Message } from '../api/generated/Message'
import type { ToolResult } from '../api/generated/ToolResult'
import type { JsonValue } from '../api/generated/serde_json/JsonValue'
import type { LiveBlock } from '../chat/state'

type Results = Record<string, ToolResult>

/** A message's blocks, rendered inside a `MessageRow`. */
export function MessageContent({
  message,
  results,
  renderText = plainText,
}: {
  message: Message
  results: Results
  renderText?: (text: string) => ReactNode
}) {
  return (
    <>
      {message.content.map((block, i) => (
        <Block key={i} block={block} results={results} renderText={renderText} />
      ))}
    </>
  )
}

function Block({
  block,
  results,
  renderText,
}: {
  block: ContentBlock
  results: Results
  renderText: (text: string) => ReactNode
}) {
  switch (block.type) {
    case 'text':
      return <div>{renderText(block.text)}</div>
    case 'thinking':
      return <Thinking text={block.thinking} />
    case 'redacted_thinking':
      return <p className="text-[13px] text-muted italic">Thinking (redacted by the provider)</p>
    case 'tool_use':
      return <ToolCall name={block.name} args={block.input} result={results[block.id]} />
    case 'image':
      return (
        <img className="max-w-md rounded-md" src={`data:${block.media_type};base64,${block.data}`} alt="" />
      )
    case 'tool_result':
      return null
  }
}

/** A run still streaming: text, thinking, and tool calls as they arrive. */
export function LiveContent({
  blocks,
  results,
  renderText = plainText,
}: {
  blocks: LiveBlock[]
  results: Results
  renderText?: (text: string) => ReactNode
}) {
  if (blocks.length === 0) {
    return <span className="animate-pulse text-muted">…</span>
  }
  return (
    <>
      {blocks.map((b, i) => {
        switch (b.kind) {
          case 'text':
            return <div key={i}>{renderText(b.text)}</div>
          case 'thinking':
            return <Thinking key={i} text={b.text} open />
          case 'tool':
            return <ToolCall key={i} name={b.name} args={b.args} result={results[b.id]} partial={b.partial} />
        }
      })}
    </>
  )
}

function plainText(text: string): ReactNode {
  return <span className="break-words whitespace-pre-wrap">{text}</span>
}

function Thinking({ text, open }: { text: string; open?: boolean }) {
  return (
    <details className="group/thinking text-[13px] text-muted" open={open}>
      <summary className="cursor-pointer list-none select-none hover:text-ink">
        <span className="inline-block transition-transform group-open/thinking:rotate-90">▸</span> Thinking
      </summary>
      <div className="mt-1 border-l-2 border-line pl-3 whitespace-pre-wrap italic">{text}</div>
    </details>
  )
}

const TOOL_STATUS = {
  running: 'text-online',
  done: 'text-muted',
  error: 'text-danger',
} as const

/** A tool call as a compact attachment; expands to its arguments and output. */
function ToolCall({
  name,
  args,
  result,
  partial,
}: {
  name: string
  args: JsonValue
  result?: ToolResult
  partial?: string
}) {
  const status = result ? (result.is_error ? 'error' : 'done') : 'running'
  return (
    <details className="group/tool max-w-2xl rounded-md border border-line bg-subtle text-[13px]">
      <summary className="flex cursor-pointer list-none items-center gap-2 px-3 py-1.5 select-none">
        <span className="text-muted transition-transform group-open/tool:rotate-90">▸</span>
        <span className="font-mono font-bold">{name}</span>
        <span className="min-w-0 flex-1 truncate font-mono text-muted">{summarizeArgs(args)}</span>
        <span className={`text-xs ${TOOL_STATUS[status]} ${status === 'running' ? 'animate-pulse' : ''}`}>
          {status}
        </span>
      </summary>
      <div className="border-t border-line px-3 py-2">
        <pre className="max-h-60 overflow-auto font-mono text-xs whitespace-pre-wrap">
          {JSON.stringify(args, null, 2)}
        </pre>
        {result ? (
          <ToolOutput result={result} />
        ) : (
          partial && (
            <pre className="mt-2 max-h-80 overflow-auto font-mono text-xs whitespace-pre-wrap">{partial}</pre>
          )
        )}
      </div>
    </details>
  )
}

function ToolOutput({ result }: { result: ToolResult }) {
  return (
    <div className={`mt-2 border-t border-line pt-2 ${result.is_error ? 'text-danger' : ''}`}>
      {result.content.map((b, i) =>
        b.type === 'image' ? (
          <img key={i} className="max-w-md rounded-md" src={`data:${b.media_type};base64,${b.data}`} alt="" />
        ) : b.type === 'text' ? (
          <pre key={i} className="max-h-80 overflow-auto font-mono text-xs whitespace-pre-wrap">
            {b.text}
          </pre>
        ) : null,
      )}
    </div>
  )
}

/** A one-line hint for a tool call, e.g. its `path` argument. */
function summarizeArgs(args: JsonValue): string {
  if (args && typeof args === 'object' && !Array.isArray(args)) {
    for (const key of ['path', 'command', 'query']) {
      const value = args[key]
      if (typeof value === 'string') return value
    }
  }
  return ''
}
