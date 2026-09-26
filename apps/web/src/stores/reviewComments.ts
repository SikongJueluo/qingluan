import { computed, ref } from 'vue'
import { defineStore } from 'pinia'
import type { ReviewAnchor, ReviewComment } from '@/components/code-review/types'
import { reviewApi } from '@/lib/review-api'

/** Displayed comment author (single-reviewer local tool, ADR-0003). */
const AUTHOR = '你'

// Thin wrapper over the daemon comment endpoints: the daemon session is
// the source of truth (in-memory, dies with the daemon). `open(id)`
// binds this store instance to one review session.
export const useReviewCommentsStore = defineStore('review-comments', () => {
  const sessionId = ref<string | null>(null)
  const comments = ref<ReviewComment[]>([])

  const count = computed(() => comments.value.length)

  function requireSession(): string {
    if (!sessionId.value) throw new Error('review session not opened')
    return sessionId.value
  }

  async function open(id: string) {
    sessionId.value = id
    comments.value = await reviewApi.listComments(id)
  }

  function countForFile(file: string) {
    return comments.value.filter((c) => c.file === file).length
  }

  function forFile(file: string) {
    return comments.value
      .filter((c) => c.file === file)
      .sort((a, b) => a.lineFrom - b.lineFrom || a.createdAt - b.createdAt)
  }

  async function add(file: string, anchor: ReviewAnchor, content: string) {
    const trimmed = content.trim()
    if (!trimmed) return
    const created = await reviewApi.createComment(requireSession(), {
      file,
      side: anchor.side,
      lineFrom: anchor.from,
      lineTo: anchor.to,
      chunkPos: anchor.chunkPos,
      author: AUTHOR,
      content: trimmed,
    })
    comments.value.push(created)
  }

  async function update(id: string, content: string) {
    const trimmed = content.trim()
    const comment = comments.value.find((c) => c.id === id)
    if (!comment || !trimmed) return
    const updated = await reviewApi.updateComment(requireSession(), id, trimmed)
    comments.value = comments.value.map((c) => (c.id === id ? updated : c))
  }

  async function remove(id: string) {
    await reviewApi.deleteComment(requireSession(), id)
    comments.value = comments.value.filter((c) => c.id !== id)
  }

  return { sessionId, comments, count, open, countForFile, forFile, add, update, remove }
})
