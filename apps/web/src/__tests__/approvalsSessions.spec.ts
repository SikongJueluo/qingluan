import { beforeEach, describe, expect, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import { formatAge, toSubmission } from '@/approvals/sessions'
import { useApprovalsStore } from '@/approvals/store'
import { reviewApi } from '@/lib/review-api'
import type { ReviewSessionSummary } from '@/lib/review-api'

vi.mock('@/lib/review-api', () => ({
  reviewApi: {
    listSessions: vi.fn<() => Promise<ReviewSessionSummary[]>>(),
    approveSession: vi.fn<(id: string) => Promise<ReviewSessionSummary>>(),
  },
}))

const NOW = 1_700_000_000_000

function summary(overrides: Partial<ReviewSessionSummary> = {}): ReviewSessionSummary {
  return {
    id: 'abc',
    root: '/home/sikongjueluo/Projects/qingluan',
    from: 'main',
    to: '@',
    createdAt: NOW - 60_000,
    status: 'open',
    approvedAt: null,
    files: 2,
    additions: 10,
    deletions: 4,
    comments: 3,
    ...overrides,
  }
}

describe('formatAge', () => {
  it('labels relative ages in Chinese units', () => {
    expect(formatAge(NOW - 30_000, NOW)).toBe('刚刚')
    expect(formatAge(NOW - 5 * 60_000, NOW)).toBe('5 分钟前')
    expect(formatAge(NOW - 3 * 3_600_000, NOW)).toBe('3 小时前')
    expect(formatAge(NOW - 2 * 86_400_000, NOW)).toBe('2 天前')
  })

  it('never shows negative ages', () => {
    expect(formatAge(NOW + 60_000, NOW)).toBe('刚刚')
  })
})

describe('toSubmission', () => {
  it('maps an open session to a pending diff submission', () => {
    const s = toSubmission(summary(), NOW)
    expect(s.id).toBe('abc')
    expect(s.type).toBe('diff')
    expect(s.status).toBe('pending')
    expect(s.repo).toBe('qingluan')
    expect(s.title).toBe('main..@')
    expect(s.range).toBe('main..@')
    expect(s.age).toBe('1 分钟前')
    expect(s.ageHours).toBeCloseTo(1 / 60)
    expect(s.files).toBe(2)
    expect(s.additions).toBe(10)
    expect(s.deletions).toBe(4)
    expect(s.comments).toBe(3)
    expect(s.approvedAt).toBeUndefined()
  })

  it('maps an approved session to done and passes approvedAt through', () => {
    const s = toSubmission(summary({ status: 'approved', approvedAt: NOW }), NOW)
    expect(s.status).toBe('done')
    expect(s.approvedAt).toBe(NOW)
  })
})

describe('useApprovalsStore', () => {
  beforeEach(() => {
    setActivePinia(createPinia())
    vi.mocked(reviewApi.listSessions).mockReset()
    vi.mocked(reviewApi.approveSession).mockReset()
  })

  it('refresh replaces items from the daemon listing', async () => {
    vi.mocked(reviewApi.listSessions).mockResolvedValue([
      summary({ id: 'new', createdAt: NOW }),
      summary({ id: 'old', createdAt: NOW - 1_000 }),
    ])
    const store = useApprovalsStore()
    await store.refresh()
    expect(store.items.map((s) => s.id)).toEqual(['new', 'old'])
    expect(store.active).toHaveLength(2)
    expect(store.error).toBeNull()
    expect(store.loaded).toBe(true)
  })

  it('keeps old data and records the error when the daemon is unreachable', async () => {
    vi.mocked(reviewApi.listSessions)
      .mockResolvedValueOnce([summary({ id: 'one' })])
      .mockRejectedValueOnce(new Error('daemon request failed (/reviews): daemon 不可达'))
    const store = useApprovalsStore()
    await store.refresh()
    await store.refresh()
    expect(store.items.map((s) => s.id)).toEqual(['one'])
    expect(store.error).toContain('不可达')
  })

  it('approve calls the daemon and updates the item in place', async () => {
    vi.mocked(reviewApi.listSessions).mockResolvedValue([summary({ id: 'one' })])
    vi.mocked(reviewApi.approveSession).mockResolvedValue(
      summary({ id: 'one', status: 'approved', approvedAt: NOW }),
    )
    const store = useApprovalsStore()
    await store.refresh()
    await store.approve('one')
    expect(reviewApi.approveSession).toHaveBeenCalledWith('one')
    expect(store.items[0]!.status).toBe('done')
    expect(store.active).toHaveLength(0)
  })
})
