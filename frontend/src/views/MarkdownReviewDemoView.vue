<script setup lang="ts">
import MarkdownReviewLayout from '@/components/markdown/MarkdownReviewLayout.vue'
import type { MarkdownBlock } from '@/markdown/types'

const blocks: MarkdownBlock[] = [
  { id: 'block-1', type: 'heading', order: 1, depth: 1, text: 'Markdown Block 审查组件 MVP' },
  {
    id: 'block-2',
    type: 'paragraph',
    order: 2,
    text: '这是一个 Notion 风格的 Markdown 阅读审查界面示例。每个段落、标题、代码块和表格都是独立 block。',
  },
  { id: 'block-3', type: 'blockquote', order: 3, text: '审查界面应该突出内容，同时让评论入口保持轻量。' },
  {
    id: 'block-4',
    type: 'code',
    order: 4,
    lang: 'ts',
    raw: 'const selected = new Set<string>()\nselected.add("block-1")',
  },
  {
    id: 'block-5',
    type: 'list',
    order: 5,
    meta: { ordered: false },
    children: [
      { id: 'block-5-1', type: 'listItem', order: 1, text: '点击 block 显示工具条' },
      { id: 'block-5-2', type: 'listItem', order: 2, text: '点击评论按钮在鼠标位置弹窗' },
      { id: 'block-5-3', type: 'listItem', order: 3, text: '提交后 block 右上角显示评论气泡' },
    ],
  },
  {
    id: 'block-6',
    type: 'table',
    order: 6,
    meta: {
      table: {
        headers: ['Block', '职责'],
        rows: [
          ['BlockFrame', '交互壳层'],
          ['MarkdownDocument', '分发渲染'],
          ['MarkdownReviewLayout', '审查布局'],
        ],
      },
    },
  },
  { id: 'block-7', type: 'image', order: 7, text: '示例图片占位', meta: { alt: '未提供图片地址' } },
  { id: 'block-8', type: 'hr', order: 8 },
  { id: 'block-9', type: 'unknown', order: 9, raw: '未来解析器输出的新 block 类型会先降级显示。' },
]
</script>

<template>
  <div class="h-full min-h-[calc(100vh-4rem)]">
    <MarkdownReviewLayout :blocks="blocks">
      <template #sidebar>
        <div class="space-y-2">
          <h2 class="text-sm font-semibold">审查侧栏</h2>
          <p class="text-sm text-muted-foreground">
            此处预留给后续「修改文档」功能。评论请在 block 上直接进行。
          </p>
        </div>
      </template>
    </MarkdownReviewLayout>
  </div>
</template>
