<script setup lang="ts">
import { computed } from 'vue'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import type { MarkdownBlock, MarkdownTableMeta } from '@/markdown/types'

const props = defineProps<{
  block: MarkdownBlock
}>()

const table = computed(() => props.block.meta?.table as MarkdownTableMeta | undefined)
</script>

<template>
  <div class="overflow-hidden rounded-md border">
    <Table v-if="table">
      <TableHeader>
        <TableRow>
          <TableHead v-for="(header, headerIndex) in table.headers" :key="`${header}-${headerIndex}`">
            {{ header }}
          </TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        <TableRow v-for="(row, rowIndex) in table.rows" :key="rowIndex">
          <TableCell v-for="(cell, cellIndex) in row" :key="cellIndex">
            {{ cell }}
          </TableCell>
        </TableRow>
      </TableBody>
    </Table>

    <pre v-else class="p-4 text-sm">{{ block.raw }}</pre>
  </div>
</template>
