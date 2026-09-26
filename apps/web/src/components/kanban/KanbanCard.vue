<script setup lang="ts">
import type { KanbanTask } from '@/kanban/types'
import { useKanbanDragDrop } from '@/composables/useKanbanDragDrop'
import { Card, CardContent, CardHeader } from '@/components/ui/card'
import { Badge } from '@/components/ui/badge'
import { cn } from '@/lib/utils'

defineProps<{
  task: KanbanTask
}>()

const { draggingTaskId, onDragStart, onDragEnd } = useKanbanDragDrop()
</script>

<template>
  <Card
    :data-card-id="task.id"
    :class="
      cn(
        'cursor-grab select-none transition-opacity active:cursor-grabbing',
        draggingTaskId === task.id && 'opacity-50',
      )
    "
    size="sm"
    draggable="true"
    @dragstart="onDragStart($event, task.id)"
    @dragend="onDragEnd()"
  >
    <CardHeader class="px-3 py-2">
      <span class="text-sm font-medium leading-snug">{{ task.title }}</span>
    </CardHeader>
    <CardContent v-if="task.participants.length" class="flex flex-wrap gap-1 px-3 pb-2 pt-0">
      <Badge v-for="participant in task.participants" :key="participant" variant="secondary">
        {{ participant }}
      </Badge>
    </CardContent>
  </Card>
</template>
