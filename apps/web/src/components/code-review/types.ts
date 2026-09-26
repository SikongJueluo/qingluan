export type ChangeStatus = 'modified' | 'added' | 'deleted'

/**
 * Per-file metadata served by `GET /reviews/{id}/files` (two-level
 * loading: the list stays cheap; full text is fetched per file).
 */
export interface ChangedFileMeta {
  path: string
  status: ChangeStatus
  additions: number
  deletions: number
  binary: boolean
}

/** Metadata plus the file contents (after `?side=` fetch). */
export interface ChangedFile extends ChangedFileMeta {
  oldText: string
  newText: string
}

/**
 * 1-based inclusive line range anchor.
 * side 'new': lines of the new file. side 'old': lines inside a deleted
 * chunk (commenting on removed code), located by chunkPos.
 */
export interface ReviewAnchor {
  side: 'new' | 'old'
  from: number
  to: number
  /** side 'old': document position of the deleted chunk widget */
  chunkPos?: number
}

export interface ReviewComment {
  id: string
  /** ChangedFile.path this comment belongs to */
  file: string
  /** Anchor: which version the commented lines belong to */
  side: 'new' | 'old'
  /** Anchor line range (selection granularity is whole lines) */
  lineFrom: number
  lineTo: number
  /** side 'old': document position of the deleted chunk widget */
  chunkPos?: number
  author: string
  content: string
  createdAt: number
  updatedAt: number
}
