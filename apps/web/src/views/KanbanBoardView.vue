<script setup lang="ts">
import { onMounted } from 'vue'
import { useKanbanBoard } from '@/composables/useKanbanBoard'
import { mockKanbanTasks } from '@/kanban/mock-data'
import KanbanBoard from '@/components/kanban/KanbanBoard.vue'

const { initBoard } = useKanbanBoard()

onMounted(() => {
  initBoard(mockKanbanTasks)
})
</script>

<template>
  <div class="flex h-[calc(100vh-4rem)] flex-col">
    <div class="flex items-center gap-2 px-4 py-3">
      <h1 class="text-lg font-semibold">任务看板</h1>
    </div>
    <div class="flex-1 overflow-hidden">
      <KanbanBoard />
    </div>
  </div>
</template>
