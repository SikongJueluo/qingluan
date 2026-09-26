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
        <span class="ml-auto font-mono text-xs text-muted-foreground">
          {{ sessionId.slice(0, 8) }} · {{ fromRev }}..{{ toRev }}
        </span>
      </div>
      <p v-if="!files.length" class="text-sm text-muted-foreground">此范围内没有变更。</p>
      <CodeReviewDiff
        v-for="(file, index) in files"
        :key="file.path"
        :file="file"
        :index="index"
        :session-id="sessionId"
      />
    </template>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { useRoute } from 'vue-router'
import { GitPullRequest } from 'lucide-vue-next'
import CodeReviewDiff from '@/components/code-review/CodeReviewDiff.vue'
import type { ChangedFileMeta } from '@/components/code-review/types'
import { reviewApi } from '@/lib/review-api'
import { useReviewCommentsStore } from '@/stores/reviewComments'

const route = useRoute()
const store = useReviewCommentsStore()

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

onMounted(async () => {
  if (!sessionId.value) {
    error.value = 'URL 缺少 review 会话 id（应由 qingluan review 生成）'
    loading.value = false
    return
  }
  try {
    files.value = await reviewApi.listFiles(sessionId.value)
    await store.open(sessionId.value)
  } catch (e) {
    error.value = e instanceof Error ? e.message : String(e)
  } finally {
    loading.value = false
  }
})
</script>
