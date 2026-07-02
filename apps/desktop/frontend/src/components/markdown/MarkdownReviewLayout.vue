<script setup lang="ts">
import {
  ResizableHandle,
  ResizablePanel,
  ResizablePanelGroup,
} from '@/components/ui/resizable'
import { ScrollArea } from '@/components/ui/scroll-area'
import type { MarkdownBlock } from '@/markdown/types'
import MarkdownDocument from './MarkdownDocument.vue'

defineProps<{
  blocks: MarkdownBlock[]
}>()
</script>

<template>
  <ResizablePanelGroup direction="horizontal" class="h-full min-h-[calc(100vh-4rem)] w-full">
    <ResizablePanel :default-size="70" :min-size="45">
      <ScrollArea class="h-full">
        <MarkdownDocument :blocks="blocks" />
      </ScrollArea>
    </ResizablePanel>

    <ResizableHandle />

    <ResizablePanel :default-size="30" :min-size="20">
      <div class="h-full border-l bg-muted/20 p-4">
        <slot name="sidebar">
          <div class="text-sm text-muted-foreground">
            后续修改文档的功能将在此处提供（MVP 占位）。
          </div>
        </slot>
      </div>
    </ResizablePanel>
  </ResizablePanelGroup>
</template>
