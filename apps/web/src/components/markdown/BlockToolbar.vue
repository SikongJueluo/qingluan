<script setup lang="ts">
import { MessageSquare, MoreHorizontal } from 'lucide-vue-next'
import { Button } from '@/components/ui/button'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import type { MarkdownBlock } from '@/markdown/types'

defineProps<{
  block: MarkdownBlock
}>()

const emit = defineEmits<{
  comment: [event: MouseEvent]
  copyText: []
  copyRaw: []
}>()
</script>

<template>
  <div class="flex items-center gap-1 rounded-md border bg-background p-1 shadow-md">
    <Button
      variant="ghost"
      size="icon-sm"
      :data-testid="`comment-${block.id}`"
      @click.stop="emit('comment', $event)"
    >
      <MessageSquare class="size-4" />
    </Button>

    <DropdownMenu>
      <DropdownMenuTrigger as-child>
        <Button variant="ghost" size="icon-sm" @click.stop>
          <MoreHorizontal class="size-4" />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start">
        <DropdownMenuItem @click="emit('copyText')"> 复制纯文本 </DropdownMenuItem>
        <DropdownMenuItem @click="emit('copyRaw')"> 复制 Markdown 原文 </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  </div>
</template>
