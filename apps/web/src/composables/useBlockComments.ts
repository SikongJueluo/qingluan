import { computed, ref } from 'vue'

export interface BlockComment {
  id: string
  blockId: string
  content: string
  createdAt: number
}

const comments = ref<BlockComment[]>([])

function makeId() {
  return `${Date.now()}-${Math.random().toString(36).slice(2, 8)}`
}

export function useBlockComments() {
  function addComment(blockId: string, content: string) {
    const trimmed = content.trim()
    if (!trimmed) return
    comments.value = [
      ...comments.value,
      { id: makeId(), blockId, content: trimmed, createdAt: Date.now() },
    ]
  }

  function commentsFor(blockId: string) {
    return computed(() => comments.value.filter((c) => c.blockId === blockId))
  }

  function reset() {
    comments.value = []
  }

  return {
    comments,
    addComment,
    commentsFor,
    reset,
  }
}
