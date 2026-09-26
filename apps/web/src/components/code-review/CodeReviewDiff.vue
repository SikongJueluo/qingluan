<template>
  <div class="overflow-hidden rounded-md border">
    <button
      type="button"
      class="flex w-full items-center gap-2 bg-muted/50 px-3 py-2 text-left"
      @click="toggle"
    >
      <ChevronRight class="size-4 transition-transform" :class="{ 'rotate-90': !collapsed }" />
      <FileCode class="size-4 text-muted-foreground" />
      <span class="font-mono text-sm">{{ file.path }}</span>
      <Badge v-if="file.status === 'added'" variant="secondary">新增</Badge>
      <Badge v-if="file.binary" variant="secondary">二进制</Badge>
      <span class="ml-auto flex items-center gap-2 text-xs">
        <span class="text-green-600">+{{ file.additions }}</span>
        <span class="text-red-600">−{{ file.deletions }}</span>
        <span v-if="commentCount" class="text-muted-foreground"> {{ commentCount }} 条评论 </span>
      </span>
    </button>
    <div v-show="!collapsed">
      <p v-if="file.binary" class="p-4 text-sm text-muted-foreground">二进制文件，不展示内容。</p>
      <p v-else-if="error" class="p-4 text-sm text-destructive">{{ error }}</p>
      <p v-else-if="!loaded" class="p-4 text-sm text-muted-foreground">正在加载文件内容 …</p>
      <div v-show="loaded" ref="editorEl" />
    </div>
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
import { computed, onBeforeUnmount, ref, watch } from 'vue'
import { ChevronRight, FileCode, MessageSquare } from 'lucide-vue-next'
import { basicSetup } from 'codemirror'
import { EditorState } from '@codemirror/state'
import { EditorView } from '@codemirror/view'
import { javascript } from '@codemirror/lang-javascript'
import { unifiedMergeView } from '@codemirror/merge'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { useReviewCommentsStore } from '@/stores/reviewComments'
import { reviewApi } from '@/lib/review-api'
import {
  formatRange,
  reviewComments,
  setReviewComments,
  type ReviewCallbacks,
  type ReviewCommentsState,
} from './cm-threads'
import type { ChangedFileMeta, ReviewAnchor } from './types'

const props = defineProps<{
  file: ChangedFileMeta
  /** Index into the session's file list (daemon text endpoint). */
  index: number
  sessionId: string
}>()

const store = useReviewCommentsStore()
const editorEl = ref<HTMLElement | null>(null)
// Collapsed by default: file text is fetched lazily on first expand
// (two-level loading keeps a 200-file review cheap).
const collapsed = ref(true)
const loaded = ref(false)
const error = ref<string | null>(null)
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

async function toggle() {
  collapsed.value = !collapsed.value
  if (!collapsed.value && !loaded.value && !fileContents) await loadContents()
}

let fileContents: { oldText: string; newText: string } | null = null

async function loadContents() {
  if (props.file.binary) {
    loaded.value = true
    return
  }
  try {
    const [oldText, newText] = await Promise.all([
      reviewApi.fileText(props.sessionId, props.index, 'old'),
      reviewApi.fileText(props.sessionId, props.index, 'new'),
    ])
    fileContents = { oldText, newText }
    loaded.value = true
    initEditor()
  } catch (e) {
    error.value = e instanceof Error ? e.message : String(e)
  }
}

function initEditor() {
  if (!editorEl.value || !fileContents || view) return
  view = new EditorView({
    parent: editorEl.value,
    doc: fileContents.newText,
    extensions: [
      basicSetup,
      EditorState.readOnly.of(true),
      EditorView.editable.of(false),
      javascript({ typescript: true }),
      unifiedMergeView({ original: fileContents.oldText, mergeControls: false }),
      reviewComments(callbacks),
    ],
  })
  pushComments()
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
  async onAdd(anchor, content) {
    draft.value = null
    try {
      await store.add(props.file.path, anchor, content)
    } catch (e) {
      error.value = e instanceof Error ? e.message : String(e)
    }
  },
  async onUpdate(id, content) {
    try {
      await store.update(id, content)
    } catch (e) {
      error.value = e instanceof Error ? e.message : String(e)
    }
  },
  async onDelete(id) {
    try {
      await store.remove(id)
    } catch (e) {
      error.value = e instanceof Error ? e.message : String(e)
    }
  },
  onDraftCancel() {
    draft.value = null
  },
}

watch([() => store.comments, draft], pushComments, { deep: true })

onBeforeUnmount(() => {
  view?.destroy()
  view = null
})
</script>
