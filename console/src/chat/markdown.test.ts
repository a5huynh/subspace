import assert from 'node:assert/strict'
import { test } from 'node:test'

import { createElement } from 'react'
import { renderToStaticMarkup } from 'react-dom/server'
import Markdown from 'react-markdown'

import { markdownOptions } from './markdown.ts'

const render = (text: string, known: string[] = []) =>
  renderToStaticMarkup(createElement(Markdown, markdownOptions(new Set(known)), text))

test('inline formatting, code, and lists render', () => {
  assert.equal(
    render('Wrote `memory/note.md` with **bold** and _em_'),
    '<p>Wrote <code>memory/note.md</code> with <strong>bold</strong> and <em>em</em></p>',
  )
  assert.equal(render('- one\n- two'), '<ul>\n<li>one</li>\n<li>two</li>\n</ul>')
  assert.equal(
    render('```rust\nfn main() {}\n```'),
    '<pre><code class="language-rust">fn main() {}\n</code></pre>',
  )
})

test('GitHub-flavored tables and strikethrough render', () => {
  const html = render('| a | b |\n|---|---|\n| 1 | 2 |\n\n~~old~~')
  assert.match(html, /<table>.*<th>a<\/th>.*<td>2<\/td>.*<\/table>/s)
  assert.match(html, /<del>old<\/del>/)
})

test('single newlines are line breaks, as in chat', () => {
  assert.equal(render('line one\nline two'), '<p>line one<br/>\nline two</p>')
})

test('known mentions become chips, but not inside code or for unknown handles', () => {
  assert.equal(
    render('@writer ping @ghost and `@writer`', ['writer']),
    '<p><span class="mention">@writer</span> ping @ghost and <code>@writer</code></p>',
  )
  assert.equal(render('@you hi', ['you']), '<p><span class="mention mention-me">@you</span> hi</p>')
})

test('links open in a new tab; raw HTML and javascript: links are not rendered', () => {
  assert.equal(
    render('see https://releases.rs'),
    '<p>see <a href="https://releases.rs" target="_blank" rel="noreferrer noopener">https://releases.rs</a></p>',
  )
  const html = render('<img src=x onerror=alert(1)> [click](javascript:alert(1))')
  assert.doesNotMatch(html, /<img|javascript:/)
  assert.match(html, /&lt;img src=x onerror=alert\(1\)&gt;/)
})
