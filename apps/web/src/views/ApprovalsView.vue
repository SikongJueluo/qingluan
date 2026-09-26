<!--
  审批收件箱（原型 E 定稿，完整变体存档见 bookmark prototype/home-ui）。
  减法版：卡片只放简略内容 + 两个按钮（同意 / 详情）。
  - 同意的语义随类型不同：diff → agent rebase 到主分支；confirm → 按建议继续；
    markdown → 定稿。详情 → 进入对应完整审查页评论后反馈给 agent。
  - 分组可切换：按仓库（默认）/ 按工作区 / 按状态。
  - 过滤来自主页统计卡（?filter=type|status）。数据源见 src/approvals/mock-data.ts。
-->
<script setup lang="ts">
import { computed, ref } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import {
  CircleQuestionMark,
  FileDiff,
  FileText,
  GitCommitHorizontal,
  ListFilter,
} from 'lucide-vue-next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { mockSubmissions } from '@/approvals/mock-data'
import {
  approveHint,
  statusLabel,
  typeLabel,
  type ReviewSubmission,
  type SubmissionStatus,
  type SubmissionType,
} from '@/approvals/types'

const route = useRoute()
const router = useRouter()

const items = ref<ReviewSubmission[]>(mockSubmissions.map((s) => ({ ...s })))

const typeIcon: Record<SubmissionType, typeof FileDiff> = {
  diff: FileDiff,
  markdown: FileText,
  confirm: CircleQuestionMark,
}

/* ---------- 过滤（来自主页统计卡的 ?filter=） ---------- */

const STATUS_KEYS: SubmissionStatus[] = ['pending', 'in-review', 'awaiting']
const TYPE_KEYS: SubmissionType[] = ['diff', 'markdown', 'confirm']

const filter = computed<string | null>(() => {
  const v = String(route.query.filter ?? '')
  return ([...STATUS_KEYS, ...TYPE_KEYS] as string[]).includes(v) ? v : null
})
const filterLabel = computed(
  () =>
    (filter.value &&
      ((STATUS_KEYS as string[]).includes(filter.value)
        ? statusLabel[filter.value as SubmissionStatus]
        : typeLabel[filter.value as SubmissionType])) ||
    null,
)
function clearFilter() {
  void router.replace({ query: { ...route.query, filter: undefined } })
}

const queue = computed(() => {
  const f = filter.value
  if (!f) return items.value.filter((s) => s.status !== 'done')
  if ((STATUS_KEYS as string[]).includes(f))
    return items.value.filter((s) => s.status === (f as SubmissionStatus))
  return items.value.filter((s) => s.type === (f as SubmissionType) && s.status !== 'done')
})

/* ---------- 分组：仓库 / 工作区 / 状态 ---------- */

type GroupMode = 'repo' | 'workspace' | 'status'
const groupMode = ref<GroupMode>('repo')
const groupModes: { key: GroupMode; label: string }[] = [
  { key: 'repo', label: '按仓库' },
  { key: 'workspace', label: '按工作区' },
  { key: 'status', label: '按状态' },
]

function groupKey(s: ReviewSubmission) {
  if (groupMode.value === 'repo') return s.repo
  if (groupMode.value === 'workspace') return s.source
  return statusLabel[s.status]
}

const groups = computed(() => {
  const map = new Map<string, ReviewSubmission[]>()
  for (const s of queue.value) {
    const k = groupKey(s)
    if (!map.has(k)) map.set(k, [])
    map.get(k)!.push(s)
  }
  return [...map.entries()].map(([key, list]) => ({ key, list }))
})

/* ---------- 动作：同意 / 详情 ---------- */

const toast = ref('')
let toastTimer: ReturnType<typeof setTimeout> | undefined
function say(msg: string) {
  toast.value = msg
  clearTimeout(toastTimer)
  toastTimer = setTimeout(() => (toast.value = ''), 2400)
}

function approve(s: ReviewSubmission) {
  s.status = 'done'
  say(`已同意 · ${approveHint[s.type]} ·「${s.title}」`)
}

// TODO(daemon): mock id 没有真实会话，接数据源后跳转对应审查页
function openDetail(s: ReviewSubmission) {
  if (s.type === 'diff') say(`打开 diff 审查 ${s.id} · ${s.range ?? ''}`)
  else if (s.type === 'markdown') say(`打开文档审查 ${s.id}`)
  else say(`展开完整问题与上下文 ${s.id}`)
}

function badgeVariant(s: ReviewSubmission) {
  return s.type === 'diff' ? 'default' : s.type === 'confirm' ? 'secondary' : 'outline'
}
function severityDot(s: ReviewSubmission) {
  return { high: 'bg-red-500', medium: 'bg-amber-500', low: 'bg-muted-foreground/40' }[s.severity]
}
</script>

<template>
  <div class="flex h-[calc(100vh-4rem)] flex-col overflow-hidden">
    <!-- 顶栏 -->
    <div class="flex flex-wrap items-center gap-3 px-6 pt-5">
      <div>
        <h1 class="text-base font-semibold">审批收件箱</h1>
        <p class="text-xs text-muted-foreground">看一眼就能定的，直接同意；拿不准的进详情</p>
      </div>
      <div class="ml-auto flex items-center gap-2">
        <Badge v-if="filterLabel" variant="outline" class="gap-1">
          <ListFilter class="size-3" />
          {{ filterLabel }}
          <button
            class="text-muted-foreground hover:text-foreground"
            title="取消过滤"
            @click="clearFilter"
          >
            ×
          </button>
        </Badge>
        <div class="flex rounded-lg border p-0.5 text-xs">
          <button
            v-for="m in groupModes"
            :key="m.key"
            class="rounded-md px-2 py-1 transition-colors"
            :class="
              groupMode === m.key
                ? 'bg-muted font-medium'
                : 'text-muted-foreground hover:text-foreground'
            "
            @click="groupMode = m.key"
          >
            {{ m.label }}
          </button>
        </div>
      </div>
    </div>

    <!-- toast -->
    <Transition>
      <div
        v-if="toast"
        class="pointer-events-none fixed top-20 left-1/2 z-40 max-w-xl -translate-x-1/2 truncate rounded-full bg-foreground px-4 py-1.5 text-xs text-background shadow-lg"
      >
        {{ toast }}
      </div>
    </Transition>

    <!-- 卡片流 -->
    <div class="mx-auto w-full max-w-3xl flex-1 space-y-5 overflow-y-auto p-6">
      <template v-for="g in groups" :key="g.key">
        <div class="sticky top-0 z-10 -mx-2 bg-background/85 px-2 py-1.5 backdrop-blur-sm">
          <div class="flex items-center gap-2 text-xs font-medium text-muted-foreground">
            <span class="h-px flex-1 bg-border" />
            {{ g.key }}
            <span class="tabular-nums">{{ g.list.length }}</span>
            <span class="h-px w-8 bg-border" />
          </div>
        </div>

        <div
          v-for="s in g.list"
          :key="s.id"
          class="group flex items-center gap-4 rounded-xl border p-4 transition-colors hover:border-foreground/25 hover:shadow-sm"
        >
          <span class="h-8 w-1 shrink-0 rounded-full" :class="severityDot(s)" />
          <div class="min-w-0 flex-1">
            <div class="flex items-center gap-2">
              <Badge :variant="badgeVariant(s)">
                <component :is="typeIcon[s.type]" data-icon="inline-start" />
                {{ typeLabel[s.type] }}
              </Badge>
              <span class="truncate text-sm font-medium">{{ s.title }}</span>
            </div>
            <div
              class="mt-1 flex flex-wrap items-center gap-x-3 gap-y-0.5 text-xs text-muted-foreground"
            >
              <span class="truncate">{{ s.source }}</span>
              <span>·</span>
              <span class="font-mono">{{ s.repo }}</span>
              <span>·</span>
              <span>{{ s.age }}</span>
              <template v-if="s.type === 'diff'">
                <span>·</span>
                <span class="flex items-center gap-1 font-mono">
                  <GitCommitHorizontal class="size-3.5" />
                  {{ s.commits }}c {{ s.range }}
                </span>
                <span class="tabular-nums">
                  <span class="text-green-600 dark:text-green-400">+{{ s.additions }}</span>
                  <span class="text-red-600 dark:text-red-400"> −{{ s.deletions }}</span>
                </span>
              </template>
              <template v-else>
                <span v-if="s.excerpt?.length">· {{ s.excerpt[0] }}</span>
              </template>
            </div>
          </div>
          <div class="flex shrink-0 items-center gap-1.5">
            <Button size="sm" variant="ghost" @click="openDetail(s)">详情</Button>
            <Button size="sm" @click="approve(s)">同意</Button>
          </div>
        </div>
      </template>

      <div
        v-if="!queue.length"
        class="flex flex-col items-center gap-2 py-16 text-muted-foreground"
      >
        <p class="text-sm font-medium text-foreground">收件箱已清空 🎉</p>
        <p class="text-xs">agent 会把新的提交推进来</p>
      </div>
    </div>
  </div>
</template>
