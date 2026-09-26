import { beforeEach, describe, expect, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'

import type { ReviewComment } from '@/components/code-review/types'
import { useReviewCommentsStore } from '@/stores/reviewComments'

/** Envelope the daemon returns. */
const ok = <T>(data: T) => ({ ok: true, data })

const comment = (over: Partial<ReviewComment> = {}): ReviewComment => ({
  id: 'c1',
  file: 'src/a.ts',
  side: 'new',
  lineFrom: 1,
  lineTo: 2,
  chunkPos: undefined,
  author: '你',
  content: 'hello',
  createdAt: 1,
  updatedAt: 1,
  ...over,
})

function mockFetchSequence(responses: unknown[]) {
  const calls: { url: string; init?: RequestInit }[] = []
  const impl = vi.fn<(input: RequestInfo | URL, init?: RequestInit) => Promise<Response>>(
    async (input: RequestInfo | URL, init?: RequestInit) => {
      calls.push({ url: String(input), init })
      const body = responses.shift()
      return new Response(JSON.stringify(body), {
        status: 200,
        headers: { 'Content-Type': 'application/json' },
      })
    },
  )
  vi.stubGlobal('fetch', impl)
  return { calls }
}

beforeEach(() => {
  setActivePinia(createPinia())
  vi.unstubAllGlobals()
})

describe('reviewComments store', () => {
  it('open() loads comments from the daemon', async () => {
    const { calls } = mockFetchSequence([ok([comment()])])
    const store = useReviewCommentsStore()

    await store.open('s1')

    expect(store.count).toBe(1)
    expect(calls[0]?.url).toBe('/reviews/s1/comments')
  })

  it('add() POSTs and appends the server-created comment', async () => {
    const created = comment({ id: 'c9', content: 'first!' })
    const { calls } = mockFetchSequence([ok([]), ok(created)])
    const store = useReviewCommentsStore()
    await store.open('s1')

    await store.add('src/a.ts', { side: 'new', from: 1, to: 2 }, '  first!  ')

    expect(store.comments).toHaveLength(1)
    expect(store.comments[0]!.id).toBe('c9')
    const [addCall] = calls.slice(1) as [{ url: string; init?: RequestInit }]
    expect(addCall!.url).toBe('/reviews/s1/comments')
    expect(addCall!.init?.method).toBe('POST')
    expect(JSON.parse(String(addCall!.init?.body)).content).toBe('first!')
  })

  it('update() PATCHes and swaps the updated comment in', async () => {
    const updated = comment({ id: 'c1', content: 'edited' })
    const { calls } = mockFetchSequence([ok([comment()]), ok(updated)])
    const store = useReviewCommentsStore()
    await store.open('s1')

    await store.update('c1', 'edited')

    expect(store.comments[0]!.content).toBe('edited')
    expect(calls[1]?.url).toBe('/reviews/s1/comments/c1')
    expect(calls[1]?.init?.method).toBe('PATCH')
  })

  it('remove() DELETEs and drops the comment locally', async () => {
    const { calls } = mockFetchSequence([ok([comment()]), ok({ deleted: true })])
    const store = useReviewCommentsStore()
    await store.open('s1')

    await store.remove('c1')

    expect(store.count).toBe(0)
    expect(calls[1]?.url).toBe('/reviews/s1/comments/c1')
    expect(calls[1]?.init?.method).toBe('DELETE')
  })

  it('mutations before open() fail fast', async () => {
    mockFetchSequence([])
    const store = useReviewCommentsStore()
    await expect(store.add('src/a.ts', { side: 'new', from: 1, to: 1 }, 'x')).rejects.toThrow(
      'review session not opened',
    )
  })

  it('forFile sorts by line then creation time', () => {
    const store = useReviewCommentsStore()
    store.comments = [
      comment({ id: 'b', lineFrom: 5, createdAt: 1 }),
      comment({ id: 'a', lineFrom: 2, createdAt: 9 }),
      comment({ id: 'c', lineFrom: 2, createdAt: 7, file: 'other.ts' }),
    ]
    expect(store.forFile('src/a.ts').map((c) => c.id)).toEqual(['a', 'b'])
    expect(store.countForFile('src/a.ts')).toBe(2)
  })
})
