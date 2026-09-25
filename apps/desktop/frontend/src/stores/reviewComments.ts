import { computed, ref } from 'vue'
import { defineStore } from 'pinia'
import type { ReviewAnchor, ReviewComment } from '@/components/code-review/types'

function makeId() {
  return `${Date.now()}-${Math.random().toString(36).slice(2, 8)}`
}

// In-memory by design: persistence / shipping comments to the agent is a
// later step (see the code-review issue). The store is the single source of
// truth so the export path only needs to read `comments` here.
export const useReviewCommentsStore = defineStore('review-comments', () => {
  const comments = ref<ReviewComment[]>([])

  const count = computed(() => comments.value.length)

  function countForFile(file: string) {
    return comments.value.filter((c) => c.file === file).length
  }

  function forFile(file: string) {
    return comments.value
      .filter((c) => c.file === file)
      .sort((a, b) => a.lineFrom - b.lineFrom || a.createdAt - b.createdAt)
  }

  function add(file: string, anchor: ReviewAnchor, content: string) {
    const trimmed = content.trim()
    if (!trimmed) return
    const now = Date.now()
    comments.value.push({
      id: makeId(),
      file,
      side: anchor.side,
      lineFrom: anchor.from,
      lineTo: anchor.to,
      chunkPos: anchor.chunkPos,
      author: '你',
      content: trimmed,
      createdAt: now,
      updatedAt: now,
    })
  }

  function update(id: string, content: string) {
    const trimmed = content.trim()
    const comment = comments.value.find((c) => c.id === id)
    if (!comment || !trimmed) return
    comment.content = trimmed
    comment.updatedAt = Date.now()
  }

  function remove(id: string) {
    comments.value = comments.value.filter((c) => c.id !== id)
  }

  return { comments, count, countForFile, forFile, add, update, remove }
})
