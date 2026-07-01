<script setup lang="ts">
import { computed } from 'vue'
import type { MarkdownBlock } from '@/markdown/types'

const props = defineProps<{
  block: MarkdownBlock
}>()

const src = computed(() => props.block.meta?.src as string | undefined)
const alt = computed(() => (props.block.meta?.alt as string | undefined) ?? props.block.text ?? '')
</script>

<template>
  <figure class="space-y-2">
    <img v-if="src" :src="src" :alt="alt" class="max-h-[420px] rounded-lg border object-contain" />
    <div v-else class="rounded-lg border border-dashed bg-muted/30 p-4 text-sm text-muted-foreground">
      图片地址缺失：{{ alt || block.raw || block.id }}
    </div>
    <figcaption v-if="alt" class="text-xs text-muted-foreground">
      {{ alt }}
    </figcaption>
  </figure>
</template>
