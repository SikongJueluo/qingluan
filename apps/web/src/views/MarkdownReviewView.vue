<!--
  Markdown 审查详情页（/markdown-review/:id）。
  与代码审查（/review/:id）同模式：不在侧边栏露出，
  由审批收件箱的「详情」进入。侧栏提供批准 / 请求修改
  （状态经 useApprovalsStore 与收件箱共享）与返回入口。
-->
<script setup lang="ts">
import { computed, ref } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { ArrowLeft, Bot, FileText } from 'lucide-vue-next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import MarkdownReviewLayout from '@/components/markdown/MarkdownReviewLayout.vue'
import { markdownDocs } from '@/approvals/mock-data'
import { useApprovalsStore } from '@/approvals/store'
import { approveHint, statusLabel, typeLabel } from '@/approvals/types'

const route = useRoute()
const router = useRouter()
const store = useApprovalsStore()

const id = computed(() => String(route.params.id ?? ''))
const submission = computed(() => store.byId(id.value))
const blocks = computed(() => markdownDocs[id.value])
const error = computed(() => {
  if (!submission.value) return `找不到审批提交 ${id.value}（应由审批收件箱进入）`
  if (!blocks.value) return `提交 ${id.value} 没有 markdown 文档内容`
  return null
})

const handled = ref(false)

function approve() {
  store.approve(id.value)
  handled.value = true
  setTimeout(() => void router.push('/approvals'), 600)
}

function requestChanges() {
  store.requestChanges(id.value)
  handled.value = true
  setTimeout(() => void router.push('/approvals'), 600)
}
</script>

<template>
  <div class="h-full min-h-[calc(100vh-4rem)]">
    <template v-if="error">
      <div class="flex flex-col items-center gap-3 py-24">
        <p class="text-sm text-muted-foreground">{{ error }}</p>
        <Button size="sm" variant="outline" @click="router.push('/approvals')">
          <ArrowLeft /> 返回审批收件箱
        </Button>
      </div>
    </template>
    <template v-else-if="submission">
      <div class="flex items-center gap-3 border-b px-6 py-3">
        <FileText class="size-5" />
        <h1 class="truncate text-lg font-semibold">{{ submission.title }}</h1>
        <Badge variant="secondary">{{ typeLabel[submission.type] }}</Badge>
        <Badge variant="outline">{{ statusLabel[submission.status] }}</Badge>
        <span class="hidden truncate text-xs text-muted-foreground lg:inline">
          {{ submission.source }} · {{ submission.repo }} · {{ submission.age }} ·
          {{ submission.blocks }} blocks · 💬 {{ submission.comments }}
        </span>
        <Button
          size="sm"
          variant="ghost"
          class="ml-auto"
          title="返回审批收件箱"
          @click="router.push('/approvals')"
        >
          <ArrowLeft /> 收件箱
        </Button>
      </div>
      <MarkdownReviewLayout :blocks="blocks ?? []">
        <template #sidebar>
          <div class="space-y-4">
            <div>
              <h2 class="flex items-center gap-1.5 text-sm font-semibold">
                <Bot class="size-4" /> {{ submission.source }}
              </h2>
              <p class="mt-1 text-xs text-muted-foreground">
                「同意」= {{ approveHint[submission.type] }}；有意见请在 block 上评论，
                完成后点「请求修改」反馈给 agent。
              </p>
            </div>
            <div class="flex flex-col gap-2">
              <Button size="sm" :disabled="handled" @click="approve">
                {{ handled ? '已同意，返回中…' : '同意' }}
              </Button>
              <Button size="sm" variant="outline" :disabled="handled" @click="requestChanges">
                请求修改
              </Button>
            </div>
          </div>
        </template>
      </MarkdownReviewLayout>
    </template>
  </div>
</template>
