<script setup lang="ts">
import { Badge } from '@/components/ui/badge'
import type { MarkdownBlock } from '@/markdown/types'

defineProps<{
  block: MarkdownBlock
}>()
</script>

<template>
  <div class="rounded-md border border-dashed bg-muted/20 p-3 text-sm">
    <div class="mb-2 flex items-center gap-2 text-muted-foreground">
      <Badge variant="secondary">{{ block.type }}</Badge>
      <span>未识别的 Markdown block</span>
    </div>
    <pre class="whitespace-pre-wrap text-foreground">{{ block.raw ?? block.text ?? block.id }}</pre>
  </div>
</template>
