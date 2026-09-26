<!--
  概览主页（原型 E 定稿，完整变体存档见 bookmark prototype/home-ui）。
  减法原则：主页只做统计 + Agent 动态，一切处理动作都在 /approvals 收件箱。
  统计卡即「面板控制器」入口：点击带过滤跳转审批页。
  只展示 daemon 可提供的计数类指标；数据源见 src/approvals/mock-data.ts。
-->
<script setup lang="ts">
import { computed } from 'vue'
import { useRouter } from 'vue-router'
import {
  Activity,
  Bot,
  CheckCheck,
  CircleQuestionMark,
  FileDiff,
  FileText,
  Inbox,
} from 'lucide-vue-next'
import { mockSubmissions } from '@/approvals/mock-data'
import {
  statusLabel,
  typeLabel,
  type ReviewSubmission,
  type SubmissionType,
} from '@/approvals/types'

const router = useRouter()
const items = mockSubmissions

const active = computed(() => items.filter((s) => s.status !== 'done'))
const done = computed(() => items.filter((s) => s.status === 'done'))

const typeIcon: Record<SubmissionType, typeof FileDiff> = {
  diff: FileDiff,
  markdown: FileText,
  confirm: CircleQuestionMark,
}

const stats = computed(() => [
  {
    label: '待审批',
    value: active.value.length,
    hint: '进入审批收件箱',
    icon: Inbox,
    filter: 'all',
    primary: true,
  },
  ...(['diff', 'confirm', 'markdown'] as const).map((t) => ({
    label: typeLabel[t],
    value: active.value.filter((s) => s.type === t).length,
    hint: t === 'diff' ? '含多 commit 合并范围' : t === 'confirm' ? '执行中提问' : '待定稿',
    icon: typeIcon[t],
    filter: t,
    primary: false,
  })),
  {
    label: '今日已批准',
    value: done.value.length,
    hint: '含 rebase 与确认',
    icon: CheckCheck,
    filter: null,
    primary: false,
  },
])

function go(filter: string | null) {
  if (!filter) return
  void router.push(filter === 'all' ? '/approvals' : `/approvals?filter=${filter}`)
}

/** 各 agent 待办分布（面板控制器：谁堵住了流水线） */
const agentLoad = computed(() => {
  const map = new Map<string, number>()
  for (const s of active.value) map.set(s.source, (map.get(s.source) ?? 0) + 1)
  return [...map.entries()].sort((a, b) => b[1] - a[1])
})
const maxAgentLoad = computed(() => Math.max(1, ...agentLoad.value.map(([, n]) => n)))

/** 近 7 日每日完成审批数（daemon 可按会话时间戳统计） */
const weekBars = [3, 5, 2, 6, 4, 7, 5]
const maxWeek = Math.max(...weekBars)

const feed = computed(() => [...items].sort((a, b) => a.ageHours - b.ageHours).slice(0, 10))

function feedVerb(s: ReviewSubmission) {
  return s.status === 'done' ? '已完成' : '提交了'
}
</script>

<template>
  <div class="flex h-[calc(100vh-4rem)] flex-col overflow-hidden">
    <div class="px-6 pt-5">
      <h1 class="text-base font-semibold">概览</h1>
      <p class="text-xs text-muted-foreground">统计只做入口，处理都在审批收件箱里</p>
    </div>

    <!-- 统计卡：点击即过滤进入审批页 -->
    <div class="grid grid-cols-5 gap-3 px-6 pt-4">
      <button
        v-for="st in stats"
        :key="st.label"
        class="rounded-xl border p-4 text-left transition-colors"
        :class="[
          st.primary ? 'bg-primary/5 ring-1 ring-primary/30' : 'hover:border-foreground/25',
          st.filter ? 'cursor-pointer' : 'cursor-default',
        ]"
        :title="st.filter ? '进入审批收件箱' : undefined"
        @click="go(st.filter)"
      >
        <div class="flex items-center gap-2 text-xs text-muted-foreground">
          <component :is="st.icon" class="size-3.5" />
          {{ st.label }}
        </div>
        <div class="mt-1 text-3xl font-bold tabular-nums">{{ st.value }}</div>
        <div class="mt-0.5 text-[11px] text-muted-foreground">{{ st.hint }}</div>
      </button>
    </div>

    <div class="flex min-h-0 flex-1 gap-6 px-6 py-4">
      <!-- 统计图表区 -->
      <div class="flex min-w-0 flex-1 flex-col gap-4">
        <div class="rounded-xl border p-4">
          <div class="text-xs font-medium text-muted-foreground">近 7 日完成审批</div>
          <div class="mt-3 flex h-20 items-end gap-2">
            <div
              v-for="(v, i) in weekBars"
              :key="i"
              class="flex flex-1 flex-col items-center gap-1"
            >
              <div
                class="w-full rounded-sm"
                :class="i === weekBars.length - 1 ? 'bg-primary' : 'bg-primary/25'"
                :style="{ height: `${(v / maxWeek) * 100}%` }"
              />
              <span class="text-[10px] text-muted-foreground">
                {{ ['一', '二', '三', '四', '五', '六', '日'][i] }}
              </span>
            </div>
          </div>
        </div>

        <div class="rounded-xl border p-4">
          <div class="text-xs font-medium text-muted-foreground">各 Agent 待办</div>
          <div class="mt-3 space-y-2">
            <div
              v-for="[source, n] in agentLoad"
              :key="source"
              class="flex items-center gap-3 text-xs"
            >
              <span class="w-36 shrink-0 truncate">
                <Bot class="mr-1 inline size-3.5 text-muted-foreground" />{{ source }}
              </span>
              <span class="h-1.5 min-w-0 flex-1 overflow-hidden rounded-full bg-muted">
                <span
                  class="block h-full rounded-full"
                  :class="n >= 3 ? 'bg-red-500/80' : 'bg-primary/70'"
                  :style="{ width: `${(n / maxAgentLoad) * 100}%` }"
                />
              </span>
              <span class="w-6 text-right tabular-nums text-muted-foreground">{{ n }}</span>
            </div>
          </div>
        </div>
      </div>

      <!-- Agent 动态 -->
      <aside class="w-96 shrink-0 overflow-y-auto rounded-xl border p-4">
        <div class="flex items-center gap-2 text-sm font-semibold">
          <Activity class="size-4" /> Agent 动态
        </div>
        <div class="relative mt-4 space-y-5 pl-1">
          <div v-for="s in feed" :key="s.id" class="relative flex gap-3 border-l pb-1 pl-4">
            <span
              class="absolute top-1 -left-[4.5px] size-2 rounded-full"
              :class="s.status === 'done' ? 'bg-muted-foreground/40' : 'bg-primary'"
            />
            <div class="min-w-0">
              <p class="text-xs leading-relaxed">
                <span class="font-medium">{{ s.source }}</span>
                <span class="text-muted-foreground"> {{ feedVerb(s) }}「{{ s.title }}」</span>
              </p>
              <p class="mt-0.5 flex items-center gap-2 text-[11px] text-muted-foreground">
                <component :is="typeIcon[s.type]" class="size-3" />
                {{ typeLabel[s.type] }} · {{ statusLabel[s.status] }} · {{ s.age }}
              </p>
            </div>
          </div>
        </div>
      </aside>
    </div>
  </div>
</template>
