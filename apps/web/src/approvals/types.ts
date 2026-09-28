/**
 * 审批流领域类型。
 *
 * 三种提交类型对应三条业务流：
 * - diff:     agent 完成任务后提交的代码变更审查（往往是多个 commit
 *   合并后的 from..to 范围，不是单个 commit）
 * - markdown: 文档审查（block 级评论）
 * - confirm:  agent 执行中提问/请求确认（看一眼即可批准）
 */

export type SubmissionType = 'diff' | 'markdown' | 'confirm'
export type SubmissionStatus = 'pending' | 'in-review' | 'awaiting' | 'done'
export type Severity = 'high' | 'medium' | 'low'

export interface DiffFile {
  path: string
  additions: number
  deletions: number
}

/** 多 commit 合并范围里的单个提交摘要。 */
export interface CommitSummary {
  hash: string
  subject: string
}

export interface ReviewSubmission {
  id: string
  type: SubmissionType
  title: string
  /** 提交方：agent + workspace */
  source: string
  repo: string
  branch?: string
  severity: Severity
  status: SubmissionStatus
  /** 展示用相对时间 */
  age: string
  /** 排序用 */
  ageHours: number
  comments: number
  /** daemon 会话的批准时间（epoch 毫秒）；未批准为 undefined。 */
  approvedAt?: number
  /** diff 专属：审查对象是 from..to 范围（往往多个 commit 合并后） */
  files?: number
  additions?: number
  deletions?: number
  /** 合并的 commit 数 */
  commits?: number
  /** 人读的范围描述，如 main..feat/data-table */
  range?: string
  /** 前 N 个 commit 摘要（折叠展示用） */
  commitList?: CommitSummary[]
  fileList?: DiffFile[]
  /** markdown / confirm 专属：简略内容（审批卡片上一眼看掉） */
  blocks?: number
  excerpt?: string[]
}

export const statusLabel: Record<SubmissionStatus, string> = {
  pending: '待审查',
  'in-review': '审查中',
  awaiting: '待批准',
  done: '已完成',
}

export const typeLabel: Record<SubmissionType, string> = {
  diff: 'Diff 审批',
  markdown: '文档审查',
  confirm: '执行确认',
}

/** 「同意」在不同类型下的语义：同意后 agent 接下来做什么。 */
export const approveHint: Record<SubmissionType, string> = {
  diff: 'agent 将 rebase 到主分支完成提交',
  markdown: '文档定稿归档',
  confirm: 'agent 按建议继续执行',
}
