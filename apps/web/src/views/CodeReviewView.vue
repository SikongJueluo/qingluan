<template>
  <div class="flex flex-1 flex-col gap-4 overflow-y-auto p-6">
    <div v-if="error" class="rounded-md border border-destructive/50 bg-destructive/10 p-4 text-sm">
      {{ error }}
    </div>
    <div v-else-if="loading" class="text-sm text-muted-foreground">正在加载 diff …</div>
    <template v-else>
      <div class="flex items-center gap-3">
        <GitPullRequest class="size-5" />
        <h1 class="text-lg font-semibold">代码审查</h1>
        <span class="text-sm text-muted-foreground">
          {{ files.length }} 个文件变更 ·
          <span class="text-green-600">+{{ totalAdditions }}</span>
          <span class="text-red-600">−{{ totalDeletions }}</span> · {{ store.count }} 条评论
        </span>
        <span class="ml-auto flex items-center gap-2 font-mono text-xs text-muted-foreground">
          {{ sessionId.slice(0, 8) }} · {{ fromRev }}..{{ toRev }}
        </span>
        <Button
          size="sm"
          variant="outline"
          :disabled="!store.count"
          title="复制为 Markdown：与 qingluan review export 同格式"
          @click="copyMarkdown"
        >
          <ClipboardCheck v-if="copied" class="size-4" />
          <Clipboard v-else class="size-4" />
          {{ copied ? '已复制' : '复制为 Markdown' }}
        </Button>
      </div>

      <!-- 单文件模式：编辑器式主区域，只渲染选中的文件（已展开）。 -->
      <template v-if="selectedMeta">
        <div class="flex items-center gap-2">
          <Button
            size="sm"
            variant="ghost"
            title="回到总览（全部文件）"
            @click="explorer.select(null)"
          >
            <ArrowLeft class="size-4" />
            总览
          </Button>
          <span class="truncate font-mono text-sm">{{ selectedMeta.path }}</span>
          <span class="shrink-0 text-xs tabular-nums">
            <span class="text-green-600">+{{ selectedMeta.additions }}</span>
            <span class="text-red-600"> −{{ selectedMeta.deletions }}</span>
          </span>
          <span
            v-if="store.countForFile(selectedMeta.path)"
            class="shrink-0 text-xs text-muted-foreground"
          >
            {{ store.countForFile(selectedMeta.path) }} 条评论
          </span>
        </div>
        <CodeReviewDiff
          :key="selectedMeta.path"
          :file="selectedMeta"
          :index="selectedIndex!"
          :session-id="sessionId"
          start-expanded
        />
      </template>

      <!-- 总览：全部文件堆叠（懒加载折叠，与文件树点击联动）。 -->
      <template v-else>
        <p v-if="!files.length" class="text-sm text-muted-foreground">此范围内没有变更。</p>
        <CodeReviewDiff
          v-for="(file, index) in files"
          :key="file.path"
          :file="file"
          :index="index"
          :session-id="sessionId"
        />
      </template>
    </template>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { ArrowLeft, Clipboard, ClipboardCheck, GitPullRequest } from 'lucide-vue-next'
import { Button } from '@/components/ui/button'
import CodeReviewDiff from '@/components/code-review/CodeReviewDiff.vue'
import type { ChangedFileMeta } from '@/components/code-review/types'
import { reviewApi } from '@/lib/review-api'
import { commentsMarkdown } from '@/lib/review-export'
import { useReviewCommentsStore } from '@/stores/reviewComments'
import { useReviewExplorerStore } from '@/stores/reviewExplorer'

const route = useRoute()
const router = useRouter()
const store = useReviewCommentsStore()
const explorer = useReviewExplorerStore()

const sessionId = computed(() => String(route.params.id ?? ''))
// From/to are informational; the diff itself was snapshotted at session
// creation by the daemon.
const fromRev = computed(() => String(route.query.from ?? 'main'))
const toRev = computed(() => String(route.query.to ?? '@'))

const files = ref<ChangedFileMeta[]>([])
const loading = ref(true)
const error = ref<string | null>(null)

const totalAdditions = computed(() => files.value.reduce((sum, f) => sum + f.additions, 0))
const totalDeletions = computed(() => files.value.reduce((sum, f) => sum + f.deletions, 0))

/* ---------- 文件树联动：单文件视图 + ?file= URL 保位 ---------- */

const selectedMeta = computed(() =>
  explorer.selectedFile ? explorer.fileByPath.get(explorer.selectedFile) : undefined,
)
const selectedIndex = computed(() =>
  selectedMeta.value ? files.value.findIndex((f) => f.path === selectedMeta.value!.path) : -1,
)

// 选中文件同步到 ?file= query：刷新 / 分享 / 返回键都能保位。
watch(
  () => explorer.selectedFile,
  (file) => {
    if (route.name !== 'code-review') return
    const current = route.query.file
    const next = file ?? undefined
    if ((current ?? undefined) !== next) {
      void router.replace({ query: { ...route.query, file: next } })
    }
  },
)

const copied = ref(false)
let copiedTimer: ReturnType<typeof setTimeout> | undefined

async function copyMarkdown() {
  try {
    await navigator.clipboard.writeText(commentsMarkdown(store.comments))
    copied.value = true
    clearTimeout(copiedTimer)
    copiedTimer = setTimeout(() => {
      copied.value = false
    }, 1500)
  } catch {
    // Clipboard API unavailable (e.g. insecure context); ignore — the
    // same document is available via `qingluan review export`.
  }
}

onMounted(async () => {
  if (!sessionId.value) {
    error.value = 'URL 缺少 review 会话 id（应由 qingluan review 生成）'
    loading.value = false
    return
  }
  try {
    files.value = await reviewApi.listFiles(sessionId.value)
    await store.open(sessionId.value)
    explorer.openSession(sessionId.value, files.value)
    // 从 URL 恢复上次看到的位置（?file=，无效路径忽略）。
    const file = route.query.file
    explorer.select(typeof file === 'string' && file ? file : null)
  } catch (e) {
    error.value = e instanceof Error ? e.message : String(e)
  } finally {
    loading.value = false
  }
})

onUnmounted(() => {
  explorer.clear()
})
</script>
