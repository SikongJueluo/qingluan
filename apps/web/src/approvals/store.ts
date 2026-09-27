/**
 * 审批流共享状态：收件箱（/approvals）与详情页
 * （/markdown-review/:id、/review/:id）共同读写。
 * 数据源目前是 mock；接 daemon 审批队列后此处换成 API 调用。
 */
import { computed, ref } from 'vue'
import { defineStore } from 'pinia'
import { mockSubmissions } from './mock-data'
import type { ReviewSubmission } from './types'

export const useApprovalsStore = defineStore('approvals', () => {
  const items = ref<ReviewSubmission[]>(mockSubmissions.map((s) => ({ ...s })))

  const active = computed(() => items.value.filter((s) => s.status !== 'done'))

  function byId(id: string): ReviewSubmission | undefined {
    return items.value.find((s) => s.id === id)
  }

  /** 同意：语义随类型（diff → agent rebase；confirm → 继续执行；markdown → 定稿）。 */
  function approve(id: string): ReviewSubmission | undefined {
    const item = byId(id)
    if (item) item.status = 'done'
    return item
  }

  /** 请求修改：进审查中，等待你在详情页评论后反馈给 agent。 */
  function requestChanges(id: string): ReviewSubmission | undefined {
    const item = byId(id)
    if (item) item.status = 'in-review'
    return item
  }

  return { items, active, byId, approve, requestChanges }
})
