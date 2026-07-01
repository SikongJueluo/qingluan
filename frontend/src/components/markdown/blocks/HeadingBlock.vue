<script setup lang="ts">
import { computed } from 'vue'
import type { MarkdownBlock } from '@/markdown/types'
import InlineText from '../InlineText.vue'

const props = defineProps<{
  block: MarkdownBlock
}>()

const depth = computed(() => Math.min(Math.max(props.block.depth ?? 1, 1), 6))
const tag = computed(() => `h${depth.value}`)
</script>

<template>
  <component
    :is="tag"
    class="scroll-m-20 font-semibold tracking-tight"
    :class="{
      'mt-8 mb-4 text-3xl': depth === 1,
      'mt-7 mb-3 text-2xl': depth === 2,
      'mt-6 mb-2 text-xl': depth === 3,
      'mt-5 mb-2 text-lg': depth >= 4,
    }"
  >
    <InlineText :text="block.text" />
  </component>
</template>
