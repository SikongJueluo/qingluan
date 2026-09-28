/**
 * 审批流共享状态：收件箱（/approvals）与主页概览共同读写。
 * 数据源是 daemon 的 review 会话列表（GET /reviews）；内存态，
 * daemon 重启即清（ADR-0003）。
 */
import { computed, ref } from 'vue'
import { defineStore } from 'pinia'
import { reviewApi } from '@/lib/review-api'
import { toSubmission } from './sessions'
import type { ReviewSubmission } from './types'

export const useApprovalsStore = defineStore('approvals', () => {
  const items = ref<ReviewSubmission[]>([])
  /** 非空 = 最近一次拉取失败（daemon 未启动 / 不可达）。 */
  const error = ref<string | null>(null)
  const loaded = ref(false)

  const active = computed(() => items.value.filter((s) => s.status !== 'done'))

  /** 拉取会话列表并整体替换；失败时保留旧数据并记录错误。 */
  async function refresh(): Promise<void> {
    try {
      const sessions = await reviewApi.listSessions()
      items.value = sessions.map(toSubmission)
      error.value = null
    } catch (e) {
      error.value = e instanceof Error ? e.message : String(e)
    } finally {
      loaded.value = true
    }
  }

  function byId(id: string): ReviewSubmission | undefined {
    return items.value.find((s) => s.id === id)
  }

  /** 同意：daemon 侧标记会话完成（内存状态，重启即清）。 */
  async function approve(id: string): Promise<ReviewSubmission> {
    const summary = await reviewApi.approveSession(id)
    const mapped = toSubmission(summary)
    const index = items.value.findIndex((s) => s.id === id)
    if (index >= 0) items.value[index] = mapped
    return mapped
  }

  /** 请求修改：进审查中，等待你在详情页评论后反馈给 agent。 */
  function requestChanges(id: string): ReviewSubmission | undefined {
    const item = byId(id)
    if (item) item.status = 'in-review'
    return item
  }

  return { items, active, error, loaded, refresh, byId, approve, requestChanges }
})
