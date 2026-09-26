import type { ReviewComment } from '@/components/code-review/types'

/**
 * Render comments as the markdown agent-handoff document.
 *
 * Mirrors `qingluan review export` exactly (crates/qingluan-cli
 * comments_markdown): `# Review comments` / `## <path>` /
 * `- [<side> L<from>-L<to>] <author>: <content>`, multiline content
 * indented two spaces.
 */
export function commentsMarkdown(comments: ReviewComment[]): string {
  const sorted = [...comments].sort(
    (a, b) => a.file.localeCompare(b.file) || a.lineFrom - b.lineFrom || a.lineTo - b.lineTo,
  )

  const out: string[] = ['# Review comments', '']
  let currentFile: string | null = null
  for (const comment of sorted) {
    if (currentFile !== comment.file) {
      if (currentFile !== null) out.push('')
      out.push(`## ${comment.file}`, '')
      currentFile = comment.file
    }
    const content = comment.content.replace(/\n/g, '\n  ')
    out.push(
      `- [${comment.side} L${comment.lineFrom}-L${comment.lineTo}] ${comment.author}: ${content}`,
    )
  }
  return out.join('\n') + '\n'
}
