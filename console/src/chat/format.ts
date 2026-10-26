// Presentation helpers shared by DMs and workspaces: times, day dividers,
// avatars, grouping consecutive messages, and readable notices.

/** "9:36 PM" (locale-aware, no leading zero). */
export function formatTime(ts: string): string {
  return new Date(ts).toLocaleTimeString([], { hour: 'numeric', minute: '2-digit' })
}

/** "Tuesday, June 3, 2026, 9:36 PM": for hover titles. */
export function formatFull(ts: string): string {
  return new Date(ts).toLocaleString([], { dateStyle: 'full', timeStyle: 'short' })
}

/** Local calendar day key, for grouping by day. */
export function dayKey(ts: string): string {
  const d = new Date(ts)
  return `${d.getFullYear()}-${d.getMonth()}-${d.getDate()}`
}

/** "Today", "Yesterday", or "Monday, June 1st"-style labels for dividers. */
export function dayLabel(ts: string, now: Date = new Date()): string {
  const key = dayKey(ts)
  if (key === dayKey(now.toISOString())) return 'Today'
  const yesterday = new Date(now)
  yesterday.setDate(now.getDate() - 1)
  if (key === dayKey(yesterday.toISOString())) return 'Yesterday'
  const d = new Date(ts)
  return d.toLocaleDateString([], {
    weekday: 'long',
    month: 'long',
    day: 'numeric',
    ...(d.getFullYear() === now.getFullYear() ? {} : { year: 'numeric' }),
  })
}

/** Up to two initials: "Research Bot" → "RB", "artoo" → "A". */
export function initials(name: string): string {
  const words = name.trim().split(/\s+/).filter(Boolean)
  if (words.length === 0) return '?'
  const letters = words.length === 1 ? [words[0][0]] : [words[0][0], words[words.length - 1][0]]
  return letters.join('').toUpperCase()
}

const AVATAR_COLORS = [
  '#e01e5a',
  '#2eb67d',
  '#ecb22e',
  '#36c5f0',
  '#9b51e0',
  '#e8912d',
  '#1264a3',
  '#cd2553',
  '#008952',
  '#4a154b',
]

/** A stable color per name, so each worker keeps its avatar color. */
export function avatarColor(name: string): string {
  let hash = 0
  for (const ch of name) hash = (hash * 31 + ch.charCodeAt(0)) >>> 0
  return AVATAR_COLORS[hash % AVATAR_COLORS.length]
}

/** Consecutive messages from one author within this window share a header. */
export const GROUP_WINDOW_MS = 5 * 60 * 1000

export interface Groupable {
  /** Who wrote it; `null` for items that break groups (notices). */
  author: string | null
  timestamp: string
}

export interface RowLayout {
  /** Show a day divider before this row. */
  dayBreak: boolean
  /** Same author as the previous row, close in time: no avatar/header. */
  continued: boolean
}

/** Slack-style layout: day dividers and grouped consecutive messages. */
export function layoutRows(items: Groupable[]): RowLayout[] {
  return items.map((item, i) => {
    const prev = i > 0 ? items[i - 1] : null
    const dayBreak = !prev || dayKey(prev.timestamp) !== dayKey(item.timestamp)
    const continued =
      !dayBreak &&
      prev !== null &&
      item.author !== null &&
      prev.author === item.author &&
      Date.parse(item.timestamp) - Date.parse(prev.timestamp) < GROUP_WINDOW_MS
    return { dayBreak, continued }
  })
}

export interface NoticeSummary {
  /** One readable line. */
  summary: string
  /** The full original text, when the summary shortened it. */
  detail: string | null
}

/**
 * Pull the human message out of an API error that embeds a JSON body, e.g.
 * `Artoo: Anthropic API error 400 Bad Request: {"error":{"message":"..."}}`.
 */
export function summarizeNotice(text: string): NoticeSummary {
  const start = text.indexOf('{')
  const end = text.lastIndexOf('}')
  if (start === -1 || end <= start) return { summary: text, detail: null }
  try {
    const body = JSON.parse(text.slice(start, end + 1)) as {
      error?: { message?: unknown }
      message?: unknown
    }
    const message = body.error?.message ?? body.message
    if (typeof message !== 'string') return { summary: text, detail: null }
    const prefix = text.slice(0, start).trim().replace(/:$/, '')
    return { summary: prefix ? `${prefix}: ${message}` : message, detail: text }
  } catch {
    return { summary: text, detail: null }
  }
}
