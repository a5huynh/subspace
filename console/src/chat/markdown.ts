// Markdown for messages: GitHub-flavored (tables, task lists, strikethrough,
// autolinks), chat-style line breaks, and @mention chips. Raw HTML is never
// rendered, and react-markdown's URL sanitizer drops `javascript:` links.

import { createElement, type ReactNode } from 'react'
import type { Options } from 'react-markdown'
import remarkGfm from 'remark-gfm'

import { OPERATOR_HANDLE, segments } from './mentions.ts'

// Minimal mdast shapes; the plugin only touches text and container nodes.
interface MdNode {
  type: string
  value?: string
  children?: MdNode[]
  data?: Record<string, unknown>
}

/** Node types whose text must stay literal (and links, to keep them whole). */
const SKIP = new Set(['code', 'inlineCode', 'link', 'linkReference', 'html'])

/**
 * A remark plugin for chat text: single newlines become line breaks (chat
 * messages aren't wrapped prose), and `@handle` for a known member becomes a
 * mention chip. Code and links are left alone.
 */
export function remarkChat(known: Set<string>) {
  return () => (tree: MdNode) => {
    rewrite(tree, known)
  }
}

function rewrite(node: MdNode, known: Set<string>) {
  if (!node.children || SKIP.has(node.type)) return
  node.children = node.children.flatMap((child) => {
    if (child.type === 'text' && child.value !== undefined) return splitText(child.value, known)
    rewrite(child, known)
    return [child]
  })
}

function splitText(value: string, known: Set<string>): MdNode[] {
  const out: MdNode[] = []
  value.split('\n').forEach((line, i) => {
    if (i > 0) out.push({ type: 'break' })
    for (const s of segments(line, known)) {
      if (s.kind === 'text') {
        if (s.text) out.push({ type: 'text', value: s.text })
      } else {
        out.push({
          type: 'mention',
          data: {
            hName: 'span',
            hProperties: {
              className: s.handle === OPERATOR_HANDLE ? 'mention mention-me' : 'mention',
            },
            hChildren: [{ type: 'text', value: s.text }],
          },
        })
      }
    }
  })
  return out
}

/** react-markdown options for a message; `known` are mentionable handles. */
export function markdownOptions(known: Set<string> = new Set()): Options {
  return {
    remarkPlugins: [remarkGfm, remarkChat(known)],
    // Raw HTML in a message is shown as text, never rendered.
    skipHtml: false,
    components: {
      a: ({ href, children }: { href?: string; children?: ReactNode }) =>
        createElement('a', { href, target: '_blank', rel: 'noreferrer noopener' }, children),
    },
  }
}
