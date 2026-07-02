<script setup lang="ts">
import { computed } from 'vue'
import type { MarkdownBlock } from '@/markdown/types'
import InlineText from '../InlineText.vue'

const props = defineProps<{
  block: MarkdownBlock
}>()

const ordered = computed(() => props.block.meta?.ordered === true || props.block.meta?.listType === 'ordered')
const tag = computed(() => (ordered.value ? 'ol' : 'ul'))
const items = computed(() => props.block.children ?? [])
</script>

<template>
  <component :is="tag" class="my-2 space-y-1 pl-6" :class="ordered ? 'list-decimal' : 'list-disc'">
    <li v-for="item in items" :key="item.id" class="leading-7">
      <InlineText :text="item.text ?? item.raw" />
    </li>
    <li v-if="items.length === 0" class="leading-7">
      <InlineText :text="block.text ?? block.raw" />
    </li>
  </component>
</template>
