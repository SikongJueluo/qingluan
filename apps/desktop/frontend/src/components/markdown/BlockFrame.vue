<script setup lang="ts">
import { computed, ref } from 'vue'
import { MessageSquare } from 'lucide-vue-next'
import { useBlockSelection } from '@/composables/useBlockSelection'
import { useBlockInteraction } from '@/composables/useBlockInteraction'
import { useBlockComments } from '@/composables/useBlockComments'
import { Button } from '@/components/ui/button'
import { Textarea } from '@/components/ui/textarea'
import type { MarkdownBlock } from '@/markdown/types'
import BlockToolbar from './BlockToolbar.vue'

const props = defineProps<{
  block: MarkdownBlock
}>()

const selection = useBlockSelection()
const interaction = useBlockInteraction()
const { commentsFor, addComment } = useBlockComments()

const selected = computed(() => selection.selectedBlockIds.value.has(props.block.id))

const {
  toolbar,
  comment,
  draft,
  openToolbar,
  openComment,
  closeComment,
  setDraft,
} = interaction

const showToolbar = computed(() => toolbar.value?.blockId === props.block.id && comment.value === null)
const commentPos = computed(() => (comment.value?.blockId === props.block.id ? comment.value : null))

const blockComments = commentsFor(props.block.id)

const showBubble = ref(false)
const bubblePos = ref<{ x: number; y: number } | null>(null)
const bubbleStyle = computed(() => ({
  left: `${bubblePos.value?.x ?? 0}px`,
  top: `${bubblePos.value?.y ?? 0}px`,
}))

function handleBlockClick(event: MouseEvent) {
  selection.selectBlock(props.block.id, { range: event.shiftKey })
  openToolbar(props.block.id, event.clientX, event.clientY)
}

function handleCommentClick(event: MouseEvent) {
  openComment(props.block.id, event.clientX, event.clientY)
}

function submitComment() {
  const content = draft.value
  if (!content.trim()) return
  addComment(props.block.id, content)
  closeComment()
}

function cancelComment() {
  closeComment()
}

function toggleBubble(event: MouseEvent) {
  if (showBubble.value) {
    showBubble.value = false
    return
  }
  bubblePos.value = { x: event.clientX, y: event.clientY }
  showBubble.value = true
}

function closeBubble() {
  showBubble.value = false
}
</script>

<template>
  <div
    :data-block-id="block.id"
    class="relative rounded-md px-3 py-1.5 transition-colors"
    :class="selected ? 'bg-primary/10 ring-1 ring-primary/30' : 'hover:bg-muted/50'"
    @click="handleBlockClick"
  >
    <slot />

    <button
      v-if="blockComments.length > 0"
      type="button"
      class="absolute right-2 top-1.5 flex items-center gap-1 rounded-full border bg-background px-1.5 py-0.5 text-xs shadow-sm hover:bg-muted"
      :data-testid="`bubble-${block.id}`"
      @click.stop="toggleBubble"
    >
      <MessageSquare class="size-3" />
      <span>{{ blockComments.length }}</span>
    </button>
  </div>

  <Teleport to="body">
    <div
      v-if="showToolbar"
      class="fixed z-40"
      :style="{ left: `${toolbar?.x ?? 0}px`, top: `${toolbar?.y ?? 0}px` }"
      @click.stop
    >
      <div class="mt-1 ml-1">
        <BlockToolbar :block="block" @comment="handleCommentClick" />
      </div>
    </div>
  </Teleport>

  <Teleport to="body">
    <template v-if="commentPos">
      <div class="fixed inset-0 z-40" @click.stop="cancelComment" />
      <div
        class="fixed z-50 w-80 rounded-md border bg-background p-3 shadow-lg"
        :style="{ left: `${commentPos.x}px`, top: `${commentPos.y}px` }"
        @click.stop
      >
        <Textarea
          v-model="draft"
          placeholder="输入评论…"
          :data-testid="`comment-input-${block.id}`"
          class="mb-2 min-h-20"
        />
        <div class="flex justify-end gap-2">
          <Button variant="ghost" size="sm" @click.stop="cancelComment">
            取消
          </Button>
          <Button
            size="sm"
            :data-testid="`comment-submit-${block.id}`"
            :disabled="!draft.trim()"
            @click.stop="submitComment"
          >
            评论
          </Button>
        </div>
      </div>
    </template>
  </Teleport>

  <Teleport to="body">
    <template v-if="showBubble">
      <div class="fixed inset-0 z-40" @click.stop="closeBubble" />
      <div
        class="fixed z-50 w-72 rounded-md border bg-background p-3 shadow-lg"
        :style="bubbleStyle"
        @click.stop
      >
        <div class="mb-2 text-sm font-medium">
          评论 ({{ blockComments.length }})
        </div>
        <ul v-if="blockComments.length" class="space-y-2 text-sm">
          <li
            v-for="c in blockComments"
            :key="c.id"
            :data-testid="`comment-item-${block.id}`"
            class="border-l-2 pl-2 text-foreground"
          >
            {{ c.content }}
          </li>
        </ul>
        <div v-else class="text-sm text-muted-foreground">暂无评论</div>
      </div>
    </template>
  </Teleport>
</template>
