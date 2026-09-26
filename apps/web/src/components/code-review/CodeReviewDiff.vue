<template>
  <div class="overflow-hidden rounded-md border">
    <button
      type="button"
      class="flex w-full items-center gap-2 bg-muted/50 px-3 py-2 text-left"
      @click="collapsed = !collapsed"
    >
      <ChevronRight class="size-4 transition-transform" :class="{ 'rotate-90': !collapsed }" />
      <FileCode class="size-4 text-muted-foreground" />
      <span class="font-mono text-sm">{{ file.path }}</span>
      <Badge v-if="file.status === 'added'" variant="secondary">新增</Badge>
      <span class="ml-auto flex items-center gap-2 text-xs">
        <span class="text-green-600">+{{ file.additions }}</span>
        <span class="text-red-600">−{{ file.deletions }}</span>
        <span v-if="commentCount" class="text-muted-foreground"> {{ commentCount }} 条评论 </span>
      </span>
    </button>
    <div v-show="!collapsed" ref="editorEl" />
    <Teleport to="body">
      <div
        v-if="popup"
        class="fixed z-50 flex items-center gap-1 rounded-md border bg-background p-1 shadow-md"
        :style="{ left: `${popup.x}px`, top: `${popup.y}px` }"
        @mousedown.prevent
      >
        <Button
          variant="ghost"
          size="icon-sm"
          :title="`评论 ${formatRange(popup.anchor)}`"
          @click="openDraftFromPopup"
        >
          <MessageSquare class="size-4" />
        </Button>
      </div>
    </Teleport>
  </div>
</template>

<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { ChevronRight, FileCode, MessageSquare } from 'lucide-vue-next'
import { basicSetup } from 'codemirror'
import { EditorState } from '@codemirror/state'
import { EditorView } from '@codemirror/view'
import { javascript } from '@codemirror/lang-javascript'
import { unifiedMergeView } from '@codemirror/merge'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { useReviewCommentsStore } from '@/stores/reviewComments'
import {
  formatRange,
  reviewComments,
  setReviewComments,
  type ReviewCallbacks,
  type ReviewCommentsState,
} from './cm-threads'
import type { ChangedFile, ReviewAnchor } from './types'

const props = defineProps<{ file: ChangedFile }>()

const store = useReviewCommentsStore()
const editorEl = ref<HTMLElement | null>(null)
const collapsed = ref(false)
const draft = ref<ReviewAnchor | null>(null)
const popup = ref<{ anchor: ReviewAnchor; x: number; y: number } | null>(null)
let view: EditorView | null = null

const commentCount = computed(() => store.countForFile(props.file.path))

function currentState(): ReviewCommentsState {
  return { comments: store.forFile(props.file.path), draft: draft.value }
}

function pushComments() {
  view?.dispatch({ effects: setReviewComments.of(currentState()) })
}

function openDraftFromPopup() {
  if (!popup.value) return
  draft.value = popup.value.anchor
  popup.value = null
  // Collapse both selections so the popup stays closed.
  window.getSelection()?.removeAllRanges()
  if (view) {
    const head = view.state.selection.main.head
    view.dispatch({ selection: { anchor: head } })
  }
}

const callbacks: ReviewCallbacks = {
  onSelectionChange(anchor, pos) {
    popup.value = anchor && pos ? { anchor, x: pos.x, y: pos.y } : null
  },
  onOpenDraft(anchor) {
    draft.value = anchor
  },
  onAdd(anchor, content) {
    draft.value = null
    store.add(props.file.path, anchor, content)
  },
  onUpdate(id, content) {
    store.update(id, content)
  },
  onDelete(id) {
    store.remove(id)
  },
  onDraftCancel() {
    draft.value = null
  },
}

onMounted(() => {
  if (!editorEl.value) return
  view = new EditorView({
    parent: editorEl.value,
    doc: props.file.newText,
    extensions: [
      basicSetup,
      EditorState.readOnly.of(true),
      EditorView.editable.of(false),
      javascript({ typescript: true }),
      unifiedMergeView({ original: props.file.oldText, mergeControls: false }),
      reviewComments(callbacks),
    ],
  })
  pushComments()
})

watch([() => store.comments, draft], pushComments, { deep: true })

onBeforeUnmount(() => {
  view?.destroy()
  view = null
})
</script>
