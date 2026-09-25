<template>
  <div class="flex flex-1 flex-col gap-4 overflow-y-auto p-6">
    <div class="flex items-center gap-3">
      <GitPullRequest class="size-5" />
      <h1 class="text-lg font-semibold">代码审查</h1>
      <span class="text-sm text-muted-foreground">
        {{ files.length }} 个文件变更 ·
        <span class="text-green-600">+{{ totalAdditions }}</span>
        <span class="text-red-600">−{{ totalDeletions }}</span> · {{ store.count }} 条评论
      </span>
      <Button class="ml-auto" size="sm" disabled title="后续版本：把评论连同 diff 一起发送给 agent">
        发送给 Agent
      </Button>
    </div>
    <CodeReviewDiff v-for="file in files" :key="file.path" :file="file" />
  </div>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import { GitPullRequest } from 'lucide-vue-next'
import { Button } from '@/components/ui/button'
import CodeReviewDiff from '@/components/code-review/CodeReviewDiff.vue'
import { stubChangedFiles } from '@/components/code-review/stub-diff'
import { useReviewCommentsStore } from '@/stores/reviewComments'

const store = useReviewCommentsStore()
const files = stubChangedFiles

const totalAdditions = computed(() => files.reduce((sum, f) => sum + f.additions, 0))
const totalDeletions = computed(() => files.reduce((sum, f) => sum + f.deletions, 0))
</script>
