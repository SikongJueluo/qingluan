<script setup lang="ts">
import type { MarkdownBlock } from '@/markdown/types'
import InlineText from '../InlineText.vue'

defineProps<{
  block: MarkdownBlock
}>()
</script>

<template>
  <p class="leading-7 text-foreground">
    <InlineText :text="block.text" />
  </p>
</template>
