<template>
  <ScrollArea class="h-full">
    <ScrollBar orientation="horizontal" />
    <div class="flex flex-row gap-4 p-4 h-full">
      <KanbanColumn
        v-for="col in columns"
        :key="col.id"
        :column="col"
        :tasks="col.tasks"
      />
    </div>
  </ScrollArea>
</template>

<script setup lang="ts">
import { ScrollArea, ScrollBar } from '@/components/ui/scroll-area'
import KanbanColumn from './KanbanColumn.vue'
import { useKanbanBoard } from '@/composables/useKanbanBoard'

const { columns } = useKanbanBoard()
</script>
