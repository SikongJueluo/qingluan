/**
 * daemon review 会话 → 审批收件箱子项的映射。
 *
 * 收件箱子项的完整字段见 ./types.ts；这里只补 daemon 能提供的部分
 * （diff 统计、相对时间），severity 等暂无真实信号的字段取保守默认。
 */
import type { ReviewSessionSummary } from '@/lib/review-api'
import type { ReviewSubmission } from './types'

/** 相对时间标签（epoch 毫秒 →「x 分钟前」）。 */
export function formatAge(createdAtMs: number, now = Date.now()): string {
  const seconds = Math.max(0, (now - createdAtMs) / 1000)
  if (seconds < 60) return '刚刚'
  const minutes = Math.floor(seconds / 60)
  if (minutes < 60) return `${minutes} 分钟前`
  const hours = Math.floor(minutes / 60)
  if (hours < 24) return `${hours} 小时前`
  return `${Math.floor(hours / 24)} 天前`
}

/** root 路径的末段作为仓库名展示（如 /home/x/Projects/qingluan → qingluan）。 */
function repoName(root: string): string {
  return root.split('/').filter(Boolean).pop() ?? root
}

/** daemon 会话摘要 → 收件箱子项。 */
export function toSubmission(s: ReviewSessionSummary, now = Date.now()): ReviewSubmission {
  return {
    id: s.id,
    type: 'diff',
    title: `${s.from}..${s.to}`,
    source: 'cli review',
    repo: repoName(s.root),
    severity: 'low',
    status: s.status === 'approved' ? 'done' : 'pending',
    age: formatAge(s.createdAt, now),
    ageHours: Math.max(0, (now - s.createdAt) / 3_600_000),
    comments: s.comments,
    approvedAt: s.approvedAt ?? undefined,
    files: s.files,
    additions: s.additions,
    deletions: s.deletions,
    range: `${s.from}..${s.to}`,
  }
}
