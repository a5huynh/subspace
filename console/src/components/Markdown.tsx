import { memo, useMemo } from 'react'
import ReactMarkdown from 'react-markdown'

import { markdownOptions } from '../chat/markdown'

const NO_HANDLES = new Set<string>()

/** A message's text rendered as Markdown; `known` handles become mention chips. */
export const Markdown = memo(function Markdown({
  text,
  known = NO_HANDLES,
}: {
  text: string
  known?: Set<string>
}) {
  const options = useMemo(() => markdownOptions(known), [known])
  return (
    <div className="markdown">
      <ReactMarkdown {...options}>{text}</ReactMarkdown>
    </div>
  )
})
