import { ref } from 'vue'

export interface BlockOverlayPos {
  blockId: string
  x: number
  y: number
}

const toolbar = ref<BlockOverlayPos | null>(null)
const comment = ref<BlockOverlayPos | null>(null)
const draft = ref('')

export function useBlockInteraction() {
  function openToolbar(blockId: string, x: number, y: number) {
    if (toolbar.value?.blockId === blockId) {
      // toggle off when clicking the same block again
      toolbar.value = null
      return
    }
    toolbar.value = { blockId, x, y }
    comment.value = null
    draft.value = ''
  }

  function closeToolbar() {
    toolbar.value = null
  }

  function openComment(blockId: string, x: number, y: number) {
    comment.value = { blockId, x, y }
    toolbar.value = null
    draft.value = ''
  }

  function closeComment() {
    comment.value = null
    draft.value = ''
  }

  function setDraft(value: string) {
    draft.value = value
  }

  function reset() {
    toolbar.value = null
    comment.value = null
    draft.value = ''
  }

  return {
    toolbar,
    comment,
    draft,
    openToolbar,
    closeToolbar,
    openComment,
    closeComment,
    setDraft,
    reset,
  }
}
