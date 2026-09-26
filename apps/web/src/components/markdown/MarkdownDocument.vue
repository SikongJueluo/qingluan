<script setup lang="ts">
import type { Component } from 'vue'
import type { MarkdownBlock } from '@/markdown/types'
import BlockFrame from './BlockFrame.vue'
import BlockquoteBlock from './blocks/BlockquoteBlock.vue'
import CodeBlock from './blocks/CodeBlock.vue'
import HeadingBlock from './blocks/HeadingBlock.vue'
import HrBlock from './blocks/HrBlock.vue'
import ImageBlock from './blocks/ImageBlock.vue'
import ListBlock from './blocks/ListBlock.vue'
import ParagraphBlock from './blocks/ParagraphBlock.vue'
import TableBlock from './blocks/TableBlock.vue'
import UnknownBlock from './blocks/UnknownBlock.vue'

defineProps<{
  blocks: MarkdownBlock[]
}>()

const renderers: Partial<Record<MarkdownBlock['type'], Component>> = {
  heading: HeadingBlock,
  paragraph: ParagraphBlock,
  code: CodeBlock,
  list: ListBlock,
  table: TableBlock,
  blockquote: BlockquoteBlock,
  image: ImageBlock,
  hr: HrBlock,
}

function getRenderer(block: MarkdownBlock): Component {
  return renderers[block.type] ?? UnknownBlock
}
</script>

<template>
  <article class="mx-auto max-w-4xl px-8 py-10">
    <BlockFrame v-for="block in blocks" :key="block.id" :block="block">
      <component :is="getRenderer(block)" :block="block" />
    </BlockFrame>
  </article>
</template>
