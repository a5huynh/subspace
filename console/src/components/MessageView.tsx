import type { ContentBlock } from '../api/generated/ContentBlock'
import type { Message } from '../api/generated/Message'
import type { ToolResult } from '../api/generated/ToolResult'
import type { JsonValue } from '../api/generated/serde_json/JsonValue'
import type { LiveBlock } from '../chat/state'

type Results = Record<string, ToolResult>

export function MessageView({ message, results }: { message: Message; results: Results }) {
  if (message.role === 'user') {
    const text = message.content
      .flatMap((b) => (b.type === 'text' ? [b.text] : []))
      .join('\n')
    return <div className="bubble user">{text}</div>
  }
  return (
    <div className="bubble assistant">
      {message.content.map((block, i) => (
        <Block key={i} block={block} results={results} />
      ))}
    </div>
  )
}

function Block({ block, results }: { block: ContentBlock; results: Results }) {
  switch (block.type) {
    case 'text':
      return <div className="text">{block.text}</div>
    case 'thinking':
      return <Thinking text={block.thinking} />
    case 'tool_use':
      return <ToolCall name={block.name} args={block.input} result={results[block.id]} />
    case 'image':
      return <img className="image" src={`data:${block.media_type};base64,${block.data}`} alt="" />
    case 'tool_result':
      return null
  }
}

export function LiveView({ blocks, results }: { blocks: LiveBlock[]; results: Results }) {
  return (
    <div className="bubble assistant live">
      {blocks.length === 0 && <div className="typing" aria-label="working">…</div>}
      {blocks.map((b, i) => {
        switch (b.kind) {
          case 'text':
            return (
              <div key={i} className="text">
                {b.text}
              </div>
            )
          case 'thinking':
            return <Thinking key={i} text={b.text} open />
          case 'tool':
            return (
              <ToolCall key={i} name={b.name} args={b.args} result={results[b.id]} partial={b.partial} />
            )
        }
      })}
    </div>
  )
}

function Thinking({ text, open }: { text: string; open?: boolean }) {
  return (
    <details className="thinking" open={open}>
      <summary>Thinking</summary>
      <div className="text">{text}</div>
    </details>
  )
}

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
    <details className={`tool tool-${status}`}>
      <summary>
        <span className="tool-name">{name}</span>
        <span className="tool-summary">{summarizeArgs(args)}</span>
        <span className="tool-status">{status}</span>
      </summary>
      <pre className="tool-args">{JSON.stringify(args, null, 2)}</pre>
      {result ? (
        <ToolOutput result={result} />
      ) : (
        partial && <pre className="tool-output">{partial}</pre>
      )}
    </details>
  )
}

function ToolOutput({ result }: { result: ToolResult }) {
  return (
    <div className={result.is_error ? 'tool-output error' : 'tool-output'}>
      {result.content.map((b, i) =>
        b.type === 'image' ? (
          <img key={i} className="image" src={`data:${b.media_type};base64,${b.data}`} alt="" />
        ) : b.type === 'text' ? (
          <pre key={i}>{b.text}</pre>
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
