import assert from 'node:assert/strict'
import { test } from 'node:test'

import { avatarColor, dayLabel, initials, layoutRows, summarizeNotice } from './format.ts'

test('initials take the first and last word', () => {
  assert.equal(initials('Research Bot'), 'RB')
  assert.equal(initials('  artoo '), 'A')
  assert.equal(initials('Big Data Helper'), 'BH')
  assert.equal(initials(''), '?')
})

test('avatar colors are stable per name', () => {
  assert.equal(avatarColor('Artoo'), avatarColor('Artoo'))
  assert.match(avatarColor('Coder'), /^#[0-9a-f]{6}$/)
})

test('day labels say Today and Yesterday', () => {
  const now = new Date(2026, 5, 10, 12, 0)
  assert.equal(dayLabel(new Date(2026, 5, 10, 8, 0).toISOString(), now), 'Today')
  assert.equal(dayLabel(new Date(2026, 5, 9, 23, 0).toISOString(), now), 'Yesterday')
  assert.doesNotMatch(dayLabel(new Date(2026, 5, 1).toISOString(), now), /Today|Yesterday|2026/)
  assert.match(dayLabel(new Date(2025, 5, 1).toISOString(), now), /2025/)
})

test('rows group by author within five minutes, and break on new days and notices', () => {
  const at = (h: number, m: number, d = 10) => new Date(2026, 5, d, h, m).toISOString()
  const rows = layoutRows([
    { author: 'you', timestamp: at(9, 0) },
    { author: 'you', timestamp: at(9, 2) }, // grouped
    { author: 'coder', timestamp: at(9, 3) }, // new author
    { author: 'coder', timestamp: at(9, 20) }, // too late to group
    { author: null, timestamp: at(9, 21) }, // a notice
    { author: 'coder', timestamp: at(9, 22) }, // after a notice: new header
    { author: 'coder', timestamp: at(9, 0, 11) }, // next day
  ])
  assert.deepEqual(
    rows.map((r) => [r.dayBreak, r.continued]),
    [
      [true, false],
      [false, true],
      [false, false],
      [false, false],
      [false, false],
      [false, false],
      [true, false],
    ],
  )
})

test('notices extract the message from embedded API error JSON', () => {
  const raw =
    'Artoo: Anthropic API error 400 Bad Request: {"type":"error","error":{"type":"invalid_request_error","message":"thinking is not supported"},"request_id":"req_1"}'
  assert.deepEqual(summarizeNotice(raw), {
    summary: 'Artoo: Anthropic API error 400 Bad Request: thinking is not supported',
    detail: raw,
  })
  assert.deepEqual(summarizeNotice('Stopped after 5 agent replies'), {
    summary: 'Stopped after 5 agent replies',
    detail: null,
  })
  assert.deepEqual(summarizeNotice('odd {not json}'), { summary: 'odd {not json}', detail: null })
})
