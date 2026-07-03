<script setup lang="ts">
import { Badge } from '@/components/ui/badge'
import type { MarkdownBlock } from '@/markdown/types'

defineProps<{
  block: MarkdownBlock
}>()
</script>

<template>
  <div class="overflow-hidden rounded-lg border bg-muted/40">
    <div class="flex items-center justify-between border-b px-3 py-1.5">
      <Badge variant="secondary">
        {{ block.lang || 'text' }}
      </Badge>
    </div>

    <pre
      class="overflow-x-auto p-4 text-sm leading-6"
    ><code>{{ block.raw ?? block.text }}</code></pre>
  </div>
</template>
