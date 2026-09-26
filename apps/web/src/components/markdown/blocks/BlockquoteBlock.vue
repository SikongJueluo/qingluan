<script setup lang="ts">
import type { MarkdownBlock } from '@/markdown/types'
import InlineText from '../InlineText.vue'

defineProps<{
  block: MarkdownBlock
}>()
</script>

<template>
  <blockquote class="border-l-4 border-muted-foreground/30 pl-4 italic text-muted-foreground">
    <InlineText :text="block.text ?? block.raw" />
  </blockquote>
</template>
