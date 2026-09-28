/**
 * Review 文件浏览器共享状态：侧边栏的文件树 tab 与主区域的单文件
 * 视图（/review/:id）共同读写。
 *
 * 会话文件由 CodeReviewView 加载后注入（openSession）；离开 review
 * 路由时清空。selectedFile 为 null 表示总览（全部文件堆叠），
 * 非 null 时主区域只渲染该文件，并同步到 ?file= query 以便刷新/
 * 分享保位。tab 是侧栏顶部「工作区 / 文件」切换。
 */
import { computed, ref } from 'vue'
import { defineStore } from 'pinia'
import type { ChangedFileMeta } from '@/components/code-review/types'

export type SidebarTab = 'workspace' | 'files'

export const useReviewExplorerStore = defineStore('reviewExplorer', () => {
  const tab = ref<SidebarTab>('workspace')
  const sessionId = ref<string | null>(null)
  const files = ref<ChangedFileMeta[]>([])
  /** null = 总览；否则为选中的文件完整路径。 */
  const selectedFile = ref<string | null>(null)

  const fileByPath = computed(() => {
    const map = new Map<string, ChangedFileMeta>()
    for (const f of files.value) map.set(f.path, f)
    return map
  })

  function setTab(value: SidebarTab) {
    tab.value = value
  }

  function openSession(id: string, list: ChangedFileMeta[]) {
    sessionId.value = id
    files.value = list
  }

  function clear() {
    sessionId.value = null
    files.value = []
    selectedFile.value = null
    // 回到工作区，避免下次进入侧栏停在无内容的文件 tab。
    tab.value = 'workspace'
  }

  /** 选中文件（null = 总览）。路径不存在时忽略，保持现状。 */
  function select(path: string | null) {
    if (path !== null && !fileByPath.value.has(path)) return
    selectedFile.value = path
  }

  return { tab, sessionId, files, selectedFile, fileByPath, setTab, openSession, clear, select }
})
