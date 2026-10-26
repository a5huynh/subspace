// @mention helpers for the channel composer and post rendering. Handles
// match the server: ASCII letters, digits, `-` and `_`, after a non-word
// character (so emails don't count).

/** The operator's handle in channels. */
export const OPERATOR_HANDLE = 'you'

export interface MentionQuery {
  /** Index of the `@`. */
  start: number
  /** What's been typed after the `@` (lowercased). */
  query: string
}

/** The mention being typed just before `caret`, if any. */
export function mentionAt(text: string, caret: number): MentionQuery | null {
  const before = text.slice(0, caret)
  const match = /(^|[^\w])@([\w-]*)$/.exec(before)
  if (!match) return null
  const start = caret - match[2].length - 1
  return { start, query: match[2].toLowerCase() }
}

/** Handles starting with the query (then containing it), at most `limit`. */
export function suggest(handles: string[], query: string, limit = 6): string[] {
  const starts = handles.filter((h) => h.startsWith(query))
  const contains = handles.filter((h) => !h.startsWith(query) && h.includes(query))
  return [...starts, ...contains].slice(0, limit)
}

/** Replace the mention at `at` with `@handle `; returns the text and new caret. */
export function complete(
  text: string,
  caret: number,
  at: MentionQuery,
  handle: string,
): { text: string; caret: number } {
  const inserted = `@${handle} `
  const after = text.slice(caret).replace(/^[\w-]*/, '').replace(/^ /, '')
  return { text: text.slice(0, at.start) + inserted + after, caret: at.start + inserted.length }
}

export type Segment = { kind: 'text'; text: string } | { kind: 'mention'; text: string; handle: string }

/** Split post text so known mentions can be highlighted. */
export function segments(text: string, known: Set<string>): Segment[] {
  const out: Segment[] = []
  const re = /(^|[^\w])@([\w-]+)/g
  let last = 0
  for (let m = re.exec(text); m; m = re.exec(text)) {
    const handle = m[2].replace(/[-_]+$/, '').toLowerCase()
    if (!known.has(handle)) continue
    const at = m.index + m[1].length
    if (at > last) out.push({ kind: 'text', text: text.slice(last, at) })
    out.push({ kind: 'mention', text: text.slice(at, at + 1 + handle.length), handle })
    last = at + 1 + handle.length
  }
  if (last < text.length) out.push({ kind: 'text', text: text.slice(last) })
  return out
}
