import type { ChangedFileMeta, ReviewComment } from '@/components/code-review/types'

/** Daemon API envelope (`qingluan_protocol::ApiResponse`). */
interface ApiResponse<T> {
  ok: boolean
  data?: T
  error?: { code: string; message: string }
}

export type ReviewSide = 'old' | 'new'

/** New comment payload (`POST /reviews/{id}/comments`). */
export interface NewReviewComment {
  file: string
  side: ReviewSide
  lineFrom: number
  lineTo: number
  chunkPos?: number
  author: string
  content: string
}

/**
 * Typed client for the daemon review endpoints.
 *
 * Same-origin relative URLs: the daemon serves this SPA, and vite dev
 * proxies `/reviews` to the daemon (see vite.config.ts).
 */
export class ReviewApi {
  constructor(private readonly base = '') {}

  private async request<T>(path: string, init?: RequestInit): Promise<T> {
    const res = await fetch(`${this.base}${path}`, {
      ...init,
      headers: { 'Content-Type': 'application/json', ...init?.headers },
    })
    const payload = (await res.json()) as ApiResponse<T>
    if (!res.ok || !payload.ok || payload.data === undefined) {
      const detail = payload.error
        ? `${payload.error.code}: ${payload.error.message}`
        : res.statusText
      throw new Error(`daemon request failed (${path}): ${detail}`)
    }
    return payload.data
  }

  createSession(path: string, from?: string, to?: string): Promise<{ id: string }> {
    return this.request('/reviews', {
      method: 'POST',
      body: JSON.stringify({ path, from, to }),
    })
  }

  listFiles(id: string): Promise<ChangedFileMeta[]> {
    return this.request(`/reviews/${id}/files`)
  }

  fileText(id: string, index: number, side: ReviewSide): Promise<string> {
    return this.request<{ path: string; text: string }>(
      `/reviews/${id}/files/${index}?side=${side}`,
    ).then((d) => d.text)
  }

  listComments(id: string): Promise<ReviewComment[]> {
    return this.request(`/reviews/${id}/comments`)
  }

  createComment(id: string, body: NewReviewComment): Promise<ReviewComment> {
    return this.request(`/reviews/${id}/comments`, {
      method: 'POST',
      body: JSON.stringify(body),
    })
  }

  updateComment(id: string, commentId: string, content: string): Promise<ReviewComment> {
    return this.request(`/reviews/${id}/comments/${commentId}`, {
      method: 'PATCH',
      body: JSON.stringify({ content }),
    })
  }

  deleteComment(id: string, commentId: string): Promise<void> {
    return this.request(`/reviews/${id}/comments/${commentId}`, {
      method: 'DELETE',
    }).then(() => undefined)
  }
}

export const reviewApi = new ReviewApi()
