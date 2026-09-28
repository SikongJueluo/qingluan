<!--
  概览主页：统计 + 最近会话动态，数据源是 daemon review 会话列表
  （GET /reviews，10s 轮询）。统计卡即入口：点击带过滤跳转审批页。
  挂了的图表（近 7 日审批量、各 Agent 待办）等 daemon 有了对应数据源
  再加回。
-->
<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted } from 'vue'
import { useRouter } from 'vue-router'
import { Activity, CheckCheck, FileDiff, Inbox } from 'lucide-vue-next'
import { useApprovalsStore } from '@/approvals/store'
import { statusLabel, typeLabel } from '@/approvals/types'

const router = useRouter()
const store = useApprovalsStore()

let pollTimer: ReturnType<typeof setInterval> | undefined
onMounted(() => {
  void store.refresh()
  pollTimer = setInterval(() => void store.refresh(), 10_000)
})
onBeforeUnmount(() => clearInterval(pollTimer))

const items = computed(() => store.items)
const active = computed(() => items.value.filter((s) => s.status !== 'done'))

const todayStart = new Date().setHours(0, 0, 0, 0)
const approvedToday = computed(
  () => items.value.filter((s) => (s.approvedAt ?? 0) >= todayStart).length,
)

const stats = computed(() => [
  {
    label: '待审批',
    value: active.value.length,
    hint: '进入审批收件箱',
    icon: Inbox,
    filter: 'all',
    primary: true,
  },
  {
    label: '累计会话',
    value: items.value.length,
    hint: 'daemon 内存态，重启即清',
    icon: FileDiff,
    filter: null,
    primary: false,
  },
  {
    label: '今日已批准',
    value: approvedToday.value,
    hint: 'cli review 触发、控制台处理',
    icon: CheckCheck,
    filter: null,
    primary: false,
  },
])

function go(filter: string | null) {
  if (!filter) return
  void router.push(filter === 'all' ? '/approvals' : `/approvals?filter=${filter}`)
}

/** 最近会话动态（新→旧） */
const feed = computed(() => [...items.value].sort((a, b) => a.ageHours - b.ageHours).slice(0, 10))

function feedVerb(s: { status: string }) {
  return s.status === 'done' ? '已完成' : '提交了'
}
</script>

<template>
  <div class="flex h-[calc(100vh-4rem)] flex-col overflow-hidden">
    <div class="px-6 pt-5">
      <h1 class="text-base font-semibold">概览</h1>
      <p class="text-xs text-muted-foreground">统计只做入口，处理都在审批收件箱里</p>
    </div>

    <!-- daemon 不可达提示 -->
    <div
      v-if="store.error"
      class="mx-6 mt-3 rounded-lg border border-amber-500/40 bg-amber-500/10 px-4 py-2 text-xs text-amber-700 dark:text-amber-400"
    >
      无法连接 daemon：{{ store.error }}（<code class="font-mono">qingluan daemon start</code>）
    </div>

    <!-- 统计卡：点击即过滤进入审批页 -->
    <div class="grid grid-cols-3 gap-3 px-6 pt-4">
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

    <!-- 最近会话动态 -->
    <div class="m-6 min-h-0 flex-1 overflow-y-auto rounded-xl border p-4">
      <div class="flex items-center gap-2 text-sm font-semibold">
        <Activity class="size-4" /> 最近会话
      </div>
      <div class="relative mt-4 space-y-5 pl-1">
        <div v-for="s in feed" :key="s.id" class="relative flex gap-3 border-l pb-1 pl-4">
          <span
            class="absolute top-1 -left-[4.5px] size-2 rounded-full"
            :class="s.status === 'done' ? 'bg-muted-foreground/40' : 'bg-primary'"
          />
          <div class="min-w-0">
            <p class="text-xs leading-relaxed">
              <span class="font-medium">{{ s.repo }}</span>
              <span class="text-muted-foreground">
                {{ feedVerb(s) }} diff 审查「{{ s.range }}」</span
              >
            </p>
            <p class="mt-0.5 flex items-center gap-2 text-[11px] text-muted-foreground">
              <FileDiff class="size-3" />
              {{ typeLabel[s.type] }} · {{ statusLabel[s.status] }} · {{ s.age }} ·
              <span v-if="s.files" class="tabular-nums">{{ s.files }} 文件</span>
            </p>
          </div>
        </div>
      </div>
      <div v-if="!feed.length" class="flex flex-col items-center gap-1 py-12 text-muted-foreground">
        <p class="text-sm">还没有 review 会话</p>
        <p class="text-xs">
          cli 执行 <code class="font-mono">qingluan review &lt;dir&gt;</code> 触发
        </p>
      </div>
    </div>
  </div>
</template>
