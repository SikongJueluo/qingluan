import { describe, expect, it } from 'vitest'

import type { ReviewComment } from '@/components/code-review/types'
import { commentsMarkdown } from '@/lib/review-export'

const comment = (over: Partial<ReviewComment>): ReviewComment => ({
  id: 'x',
  file: 'src/a.ts',
  side: 'new',
  lineFrom: 1,
  lineTo: 1,
  author: '你',
  content: 'hello',
  createdAt: 0,
  updatedAt: 0,
  ...over,
})

describe('commentsMarkdown', () => {
  it('groups by file and sorts by line, matching the CLI export format', () => {
    const md = commentsMarkdown([
      comment({ file: 'src/b.ts', lineFrom: 5, lineTo: 5, content: 'second' }),
      comment({ file: 'src/a.ts', lineFrom: 10, lineTo: 12, content: 'later' }),
      comment({ file: 'src/a.ts', side: 'old', lineFrom: 1, lineTo: 3, content: 'first' }),
    ])
    expect(md).toBe(
      [
        '# Review comments',
        '',
        '## src/a.ts',
        '',
        '- [old L1-L3] 你: first',
        '- [new L10-L12] 你: later',
        '',
        '## src/b.ts',
        '',
        '- [new L5-L5] 你: second',
        '',
      ].join('\n'),
    )
  })

  it('indents multiline content two spaces', () => {
    const md = commentsMarkdown([comment({ content: 'line one\nline two' })])
    expect(md).toContain('- [new L1-L1] 你: line one\n  line two\n')
  })

  it('renders a header-only document when empty', () => {
    expect(commentsMarkdown([])).toBe('# Review comments\n\n')
  })
})
