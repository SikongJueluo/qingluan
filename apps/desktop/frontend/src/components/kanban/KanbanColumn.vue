<script setup lang="ts">
import type { KanbanColumnDef, KanbanTask, TaskStatus } from '@/kanban/types'
import { useKanbanDragDrop } from '@/composables/useKanbanDragDrop'
import { ScrollArea } from '@/components/ui/scroll-area'
import { Badge } from '@/components/ui/badge'
import KanbanCard from './KanbanCard.vue'
import { useTemplateRef } from 'vue'
import { cn } from '@/lib/utils'

defineProps<{
  column: KanbanColumnDef
  tasks: KanbanTask[]
}>()

const { dragOverColumn, onDragEnter, onDragOver, onDragLeave, onDrop } = useKanbanDragDrop()

const cardListRef = useTemplateRef<HTMLElement>('cardList')

function handleDrop(event: DragEvent, columnId: TaskStatus) {
  const container = cardListRef.value
  if (!container) return
  onDrop(event, columnId, container)
}
</script>

<template>
  <div class="flex h-full w-[320px] min-w-[320px] flex-col rounded-lg bg-muted/50">
    <!-- Column header -->
    <div class="flex items-center gap-2 px-3 py-2.5">
      <span class="text-sm font-semibold">{{ column.title }}</span>
      <Badge variant="outline" class="ml-auto text-xs tabular-nums">{{ tasks.length }}</Badge>
    </div>

    <div class="border-b border-border/40" />

    <!-- Card list (drop zone) -->
    <ScrollArea class="flex-1">
      <div
        ref="cardList"
        :class="cn(
          'flex min-h-[80px] flex-col gap-2 p-2 transition-all',
          dragOverColumn === column.id && 'ring-2 ring-inset ring-primary/30 rounded-md',
        )"
        @dragenter="onDragEnter($event, column.id)"
        @dragover="onDragOver($event)"
        @dragleave="onDragLeave($event)"
        @drop="handleDrop($event, column.id)"
      >
        <KanbanCard
          v-for="task in tasks"
          :key="task.id"
          :task="task"
        />
      </div>
    </ScrollArea>
  </div>
</template>
