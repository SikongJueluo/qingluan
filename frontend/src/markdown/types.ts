export type MarkdownBlockType =
  | 'heading'
  | 'paragraph'
  | 'blockquote'
  | 'code'
  | 'list'
  | 'listItem'
  | 'table'
  | 'image'
  | 'hr'
  | 'html'
  | 'unknown'

export interface MarkdownBlock {
  id: string
  type: MarkdownBlockType
  order: number

  depth?: number
  text?: string
  raw?: string
  lang?: string

  startLine?: number
  endLine?: number
  startOffset?: number
  endOffset?: number

  children?: MarkdownBlock[]
  meta?: Record<string, unknown>
}

export interface MarkdownTableMeta {
  headers: string[]
  rows: string[][]
}

export interface BlockCommentDraft {
  blockId: string
  content: string
}
