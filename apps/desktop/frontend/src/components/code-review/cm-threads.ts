import {
  Prec,
  RangeSet,
  RangeSetBuilder,
  StateEffect,
  StateField,
  type Extension,
  type Text,
} from '@codemirror/state'
import {
  Decoration,
  EditorView,
  GutterMarker,
  ViewPlugin,
  WidgetType,
  gutter,
  type DecorationSet,
} from '@codemirror/view'
import type { ReviewAnchor, ReviewComment } from './types'

export interface ReviewCommentsState {
  /** Comments of a single file (the document this editor shows) */
  comments: ReviewComment[]
  /** The range with an open draft comment box, if any */
  draft: ReviewAnchor | null
}

export interface ReviewCallbacks {
  /** Selection changed (line-granular); pos is viewport coords of the popup anchor */
  onSelectionChange: (anchor: ReviewAnchor | null, pos: { x: number; y: number } | null) => void
  onOpenDraft: (anchor: ReviewAnchor) => void
  onAdd: (anchor: ReviewAnchor, content: string) => void
  onUpdate: (id: string, content: string) => void
  onDelete: (id: string) => void
  onDraftCancel: () => void
}

export const setReviewComments = StateEffect.define<ReviewCommentsState>()

const EMPTY_STATE: ReviewCommentsState = { comments: [], draft: null }

function formatTime(ts: number) {
  return new Date(ts).toLocaleString('zh-CN', {
    month: 'numeric',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
    hour12: false,
  })
}

export function formatRange(anchor: ReviewAnchor) {
  const range =
    anchor.from === anchor.to ? `第 ${anchor.from} 行` : `第 ${anchor.from}–${anchor.to} 行`
  return anchor.side === 'old' ? `已删除的${range}` : range
}

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  className: string,
  text?: string,
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag)
  node.className = className
  if (text !== undefined) node.textContent = text
  return node
}

const CARD_CLASS =
  'cm-review-thread mx-2 my-1 rounded-md border bg-card font-sans text-card-foreground shadow-sm'
const INPUT_CLASS =
  'w-full rounded-md border border-input bg-background px-2 py-1 font-sans text-sm outline-none focus-visible:ring-2 focus-visible:ring-ring/50'
const PRIMARY_BUTTON_CLASS =
  'rounded-md bg-primary px-2 py-1 text-xs text-primary-foreground hover:bg-primary/90'
const GHOST_BUTTON_CLASS =
  'rounded-md px-2 py-1 text-xs text-muted-foreground hover:text-foreground'

/** A single saved comment rendered below its anchor range. No replies: comments are read only by the agent. */
class CommentWidget extends WidgetType {
  constructor(
    readonly comment: ReviewComment,
    readonly cb: ReviewCallbacks,
  ) {
    super()
  }

  override eq(other: CommentWidget) {
    return JSON.stringify(other.comment) === JSON.stringify(this.comment)
  }

  override get estimatedHeight() {
    return 80
  }

  override ignoreEvent() {
    return true
  }

  toDOM() {
    const container = el('div', CARD_CLASS + ' cm-review-comment-card')
    const wrapper = el('div', 'flex flex-col gap-1 px-3 py-2')

    const header = el('div', 'flex items-center gap-2')
    header.appendChild(el('span', 'text-xs font-medium', this.comment.author))
    header.appendChild(
      el(
        'span',
        'text-xs text-muted-foreground',
        formatRange({
          side: this.comment.side,
          from: this.comment.lineFrom,
          to: this.comment.lineTo,
        }),
      ),
    )
    header.appendChild(
      el('span', 'text-xs text-muted-foreground', formatTime(this.comment.createdAt)),
    )
    if (this.comment.updatedAt !== this.comment.createdAt) {
      header.appendChild(el('span', 'text-xs text-muted-foreground', '（已编辑）'))
    }

    const body = el('div', 'text-sm whitespace-pre-wrap', this.comment.content)

    const edit = el('button', GHOST_BUTTON_CLASS + ' ml-auto', '编辑')
    edit.type = 'button'
    const remove = el('button', GHOST_BUTTON_CLASS, '删除')
    remove.type = 'button'
    header.append(edit, remove)

    remove.addEventListener('click', () => this.cb.onDelete(this.comment.id))
    edit.addEventListener('click', () => {
      const textarea = el('textarea', INPUT_CLASS)
      textarea.value = this.comment.content
      textarea.rows = 3
      const actions = el('div', 'flex justify-end gap-1')
      const cancel = el('button', GHOST_BUTTON_CLASS, '取消')
      cancel.type = 'button'
      const save = el('button', PRIMARY_BUTTON_CLASS, '保存')
      save.type = 'button'
      actions.append(cancel, save)
      body.replaceChildren(textarea, actions)
      edit.remove()
      remove.remove()
      textarea.focus()
      cancel.addEventListener('click', () => {
        body.replaceChildren(document.createTextNode(this.comment.content))
        header.append(edit, remove)
      })
      save.addEventListener('click', () => this.cb.onUpdate(this.comment.id, textarea.value))
    })

    wrapper.append(header, body)
    container.appendChild(wrapper)
    return container
  }
}

/** The open draft comment box under its anchor range. */
class DraftWidget extends WidgetType {
  constructor(
    readonly anchor: ReviewAnchor,
    readonly cb: ReviewCallbacks,
  ) {
    super()
  }

  override eq(other: DraftWidget) {
    return other.anchor.from === this.anchor.from && other.anchor.to === this.anchor.to
  }

  override get estimatedHeight() {
    return 110
  }

  override ignoreEvent() {
    return true
  }

  toDOM() {
    const container = el('div', CARD_CLASS)
    const box = el('div', 'flex flex-col gap-2 px-3 py-2')
    box.appendChild(el('div', 'text-xs text-muted-foreground', `评论 ${formatRange(this.anchor)}`))
    const textarea = el('textarea', INPUT_CLASS)
    textarea.placeholder = '添加评论…'
    textarea.rows = 2
    const actions = el('div', 'flex justify-end gap-1')
    const cancel = el('button', GHOST_BUTTON_CLASS, '取消')
    cancel.type = 'button'
    cancel.addEventListener('click', () => this.cb.onDraftCancel())
    const submit = el('button', PRIMARY_BUTTON_CLASS, '评论')
    submit.type = 'button'
    submit.addEventListener('click', () => this.cb.onAdd(this.anchor, textarea.value))
    actions.append(cancel, submit)
    box.append(textarea, actions)
    container.appendChild(box)
    // Focus after mount so the user can type right away.
    requestAnimationFrame(() => textarea.focus())
    return container
  }
}

class CommentedLineGutterClass extends GutterMarker {
  override elementClass = 'cm-review-comment-bar'
}

class DraftLineGutterClass extends GutterMarker {
  override elementClass = 'cm-review-draft-bar'
}

const commentedLineGutterClass = new CommentedLineGutterClass()
const draftLineGutterClass = new DraftLineGutterClass()

class AddCommentMarker extends GutterMarker {
  constructor(
    readonly line: number,
    readonly commentCount: number,
    readonly onOpenDraft: (anchor: ReviewAnchor) => void,
  ) {
    super()
  }

  override eq(other: AddCommentMarker) {
    return other.commentCount === this.commentCount
  }

  override toDOM() {
    const button = el(
      'button',
      this.commentCount ? 'cm-add-comment-marker has-comments' : 'cm-add-comment-marker',
      this.commentCount ? String(this.commentCount) : '+',
    )
    button.type = 'button'
    button.title = this.commentCount ? `${this.commentCount} 条评论` : '评论本行'
    button.addEventListener('mousedown', (event) => {
      event.preventDefault()
      event.stopPropagation()
      if (this.commentCount === 0) {
        this.onOpenDraft({ side: 'new', from: this.line, to: this.line })
      }
    })
    return button
  }
}

/**
 * GitHub-style review comments for a read-only editor:
 * - drag-select lines → floating comment popup (line granularity)
 * - gutter "+" button comments a single line
 * - comments render as block widgets below their anchor range, with the
 *   range highlighted; no replies (comments are only read by the agent)
 */
const clampRange = (doc: Text, anchor: { from: number; to: number }) => ({
  from: Math.max(1, anchor.from),
  to: Math.min(anchor.to, doc.lines),
})

const clampPos = (doc: Text, pos: number | undefined) =>
  Math.min(Math.max(0, pos ?? doc.length), doc.length)

function buildDecorations(
  doc: Text,
  state: ReviewCommentsState,
  cb: ReviewCallbacks,
): DecorationSet {
  const builder = new RangeSetBuilder<Decoration>()
  const entries: { pos: number; deco: Decoration }[] = []

  // side 1 renders after the deletion widget (side -1) at the same position,
  // so old-side comment cards land right below their deleted chunk.
  const pushWidget = (pos: number, widget: WidgetType) => {
    entries.push({ pos, deco: Decoration.widget({ widget, block: true, side: 1 }) })
  }
  const pushAnchorWidget = (anchor: ReviewAnchor, widget: WidgetType) => {
    if (anchor.side === 'old') {
      pushWidget(clampPos(doc, anchor.chunkPos), widget)
    } else {
      pushWidget(doc.line(clampRange(doc, anchor).to).to, widget)
    }
  }

  // Committed comments get NO content highlight on new-side lines — the diff
  // colors stay untouched and the comment card + comment bar carry the
  // signal. Old-side (deleted) lines are highlighted via the DOM plugin below.
  for (const comment of state.comments) {
    pushAnchorWidget(
      {
        side: comment.side,
        from: comment.lineFrom,
        to: comment.lineTo,
        chunkPos: comment.chunkPos,
      },
      new CommentWidget(comment, cb),
    )
  }

  // The open draft range is emphasized with an accent bar + translucent
  // tint (box-shadow overlay, so added/deleted diff backgrounds show
  // through) plus top/bottom bracket lines on the first/last row.
  if (state.draft) {
    if (state.draft.side === 'new') {
      const range = clampRange(doc, state.draft)
      for (let l = range.from; l <= range.to; l++) {
        entries.push({ pos: doc.line(l).from, deco: draftLineDeco(l, range) })
      }
    }
    pushAnchorWidget(state.draft, new DraftWidget(state.draft, cb))
  }

  entries.sort((a, b) => a.pos - b.pos)
  for (const { pos, deco } of entries) {
    builder.add(pos, pos, deco)
  }
  return builder.finish()
}

// The comment bar gutter's line markers only cover new-side anchors (a
// deleted chunk has no per-line gutter cells); widget rows are painted via
// the gutter's widgetMarker instead.
function draftLineDeco(line: number, range: { from: number; to: number }) {
  const cls = ['cm-review-draft-range']
  if (line === range.from) cls.push('cm-review-range-start')
  if (line === range.to) cls.push('cm-review-range-end')
  return Decoration.line({ class: cls.join(' ') })
}

function buildGutterClasses(doc: Text, state: ReviewCommentsState): RangeSet<GutterMarker> {
  const builder = new RangeSetBuilder<GutterMarker>()
  const entries: { pos: number; marker: GutterMarker }[] = []
  const pushRange = (anchor: ReviewAnchor, marker: GutterMarker) => {
    const range = clampRange(doc, anchor)
    for (let l = range.from; l <= range.to; l++) {
      entries.push({ pos: doc.line(l).from, marker })
    }
  }
  for (const comment of state.comments) {
    if (comment.side === 'new') {
      pushRange(
        { side: 'new', from: comment.lineFrom, to: comment.lineTo },
        commentedLineGutterClass,
      )
    }
  }
  if (state.draft?.side === 'new') {
    pushRange(state.draft, draftLineGutterClass)
  }
  entries.sort((a, b) => a.pos - b.pos)
  for (const { pos, marker } of entries) {
    builder.add(pos, pos, marker)
  }
  return builder.finish()
}

// Deleted chunks are widget DOM outside the document, so a drag selection
// over them never reaches the CM selection — detect it on mouseup via the
// DOM selection and anchor the comment to that chunk (side 'old').
function deletedSelectionAnchor(view: EditorView): { anchor: ReviewAnchor; rect: DOMRect } | null {
  const sel = window.getSelection()
  if (!sel || sel.isCollapsed || sel.rangeCount === 0) return null
  const range = sel.getRangeAt(0)
  const node = range.commonAncestorContainer
  const el = node instanceof Element ? node : node.parentElement
  const chunkEl = el?.closest('.cm-deletedChunk')
  if (!chunkEl || !view.dom.contains(chunkEl)) return null
  let pos: number
  try {
    pos = view.posAtDOM(chunkEl)
  } catch {
    return null
  }
  const lines = [...chunkEl.querySelectorAll('.cm-deletedLine')]
  if (lines.length === 0) return null
  let from = -1
  let to = -1
  lines.forEach((lineEl, i) => {
    if (range.intersectsNode(lineEl)) {
      if (from < 0) from = i + 1
      to = i + 1
    }
  })
  if (from < 0) return null
  return { anchor: { side: 'old', chunkPos: pos, from, to }, rect: range.getBoundingClientRect() }
}

function markDeletedLines(lines: NodeListOf<Element>, from: number, to: number, cls: string) {
  for (let i = from; i <= to; i++) {
    lines.item(i - 1)?.classList.add(cls)
  }
}

function highlightDeletedChunk(view: EditorView, chunkEl: Element, state: ReviewCommentsState) {
  let pos: number
  try {
    pos = view.posAtDOM(chunkEl)
  } catch {
    return
  }
  const lines = chunkEl.querySelectorAll('.cm-deletedLine')
  // Committed old-side comments leave NO content highlight: the gutter bar
  // (widgetMarker) + comment card carry the signal, same as new-side.
  if (state.draft?.side === 'old' && state.draft.chunkPos === pos) {
    markDeletedLines(lines, state.draft.from, state.draft.to, 'cm-review-old-draft')
  }
  // A new-side draft range spanning this chunk paints the chunk ROOT, so the
  // accent bar aligns with the bars on regular lines (deleted lines are
  // inset 6px by the chunk's padding) and the tint alpha matches.
  if (state.draft?.side === 'new') {
    const doc = view.state.doc
    const range = clampRange(doc, state.draft)
    if (pos >= doc.line(range.from).from && pos <= doc.line(range.to).to) {
      chunkEl.classList.add('cm-review-chunk-draft')
    }
  }
}

// Highlights commented/draft deleted lines. The deletion widgets are merge's
// own DOM, so classes are toggled directly (positions resolved via posAtDOM)
// instead of going through decorations.
function highlightDeletedChunks(view: EditorView, state: ReviewCommentsState) {
  view.dom.querySelectorAll('.cm-deletedLine').forEach((lineEl) => {
    lineEl.classList.remove('cm-review-old-comment', 'cm-review-old-draft')
  })
  view.dom.querySelectorAll('.cm-deletedChunk').forEach((chunkEl) => {
    chunkEl.classList.remove('cm-review-chunk-draft')
    highlightDeletedChunk(view, chunkEl, state)
  })
}

function deletedChunkHighlighter(commentsState: StateField<ReviewCommentsState>) {
  return ViewPlugin.fromClass(
    class {
      constructor(readonly view: EditorView) {
        requestAnimationFrame(() => this.refresh())
      }

      update() {
        this.refresh()
      }

      refresh() {
        highlightDeletedChunks(this.view, this.view.state.field(commentsState))
      }
    },
  )
}

function selectionPopupListener(cb: ReviewCallbacks) {
  return EditorView.updateListener.of((update) => {
    if (
      !update.selectionSet &&
      !update.docChanged &&
      !update.viewportChanged &&
      !update.geometryChanged
    ) {
      return
    }
    const sel = update.state.selection.main
    if (sel.empty) {
      cb.onSelectionChange(null, null)
      return
    }
    const doc = update.state.doc
    const from = doc.lineAt(sel.from).number
    let to = doc.lineAt(sel.to).number
    // A selection ending exactly at a line start excludes that line.
    if (to > from && sel.to === doc.line(to).from) to -= 1
    const coords = update.view.coordsAtPos(sel.head)
    cb.onSelectionChange(
      { side: 'new', from, to },
      coords ? { x: coords.left, y: coords.bottom + 6 } : null,
    )
  })
}

export function reviewComments(cb: ReviewCallbacks): Extension {
  const commentsState = StateField.define<ReviewCommentsState>({
    create: () => EMPTY_STATE,
    update(value, tr) {
      for (const effect of tr.effects) {
        if (effect.is(setReviewComments)) return effect.value
      }
      return value
    },
  })

  const commentDecorations = StateField.define<DecorationSet>({
    create: () => Decoration.none,
    update(deco, tr) {
      for (const effect of tr.effects) {
        if (effect.is(setReviewComments)) {
          return buildDecorations(tr.state.doc, effect.value, cb)
        }
      }
      return deco.map(tr.changes)
    },
    provide: (field) => EditorView.decorations.from(field),
  })

  const commentGutterClasses = StateField.define<RangeSet<GutterMarker>>({
    create: () => RangeSet.empty,
    update(value, tr) {
      for (const effect of tr.effects) {
        if (effect.is(setReviewComments)) {
          return buildGutterClasses(tr.state.doc, effect.value)
        }
      }
      return value.map(tr.changes)
    },
  })

  // A dedicated 3px gutter mirroring @codemirror/merge's change bar:
  // contiguous cells form one continuous band over the comment range.
  // Prec.high places it left of the line-number gutter.
  const commentBarGutter = Prec.high(
    gutter({
      class: 'cm-commentGutter',
      markers: (view) => view.state.field(commentGutterClasses),
      // Block-widget rows (deleted chunks, comment cards) get a bar too when
      // they sit inside an anchor range, so the band never breaks.
      widgetMarker: (view, _widget, block) => {
        const state = view.state.field(commentsState)
        const doc = view.state.doc
        const inRange = (anchor: ReviewAnchor) => {
          if (anchor.side === 'old') return anchor.chunkPos === block.from
          const range = clampRange(doc, anchor)
          return block.from >= doc.line(range.from).from && block.from <= doc.line(range.to).to
        }
        for (const comment of state.comments) {
          if (
            inRange({
              side: comment.side,
              from: comment.lineFrom,
              to: comment.lineTo,
              chunkPos: comment.chunkPos,
            })
          ) {
            return commentedLineGutterClass
          }
        }
        if (state.draft && inRange(state.draft)) return draftLineGutterClass
        return null
      },
    }),
  )

  const addCommentGutter = gutter({
    class: 'cm-review-gutter',
    lineMarker(view, line) {
      const lineNo = view.state.doc.lineAt(line.from).number
      const state = view.state.field(commentsState)
      const count = state.comments.filter((c) => c.lineTo === lineNo).length
      return new AddCommentMarker(lineNo, count, cb.onOpenDraft)
    },
    lineMarkerChange: (update) =>
      update.transactions.some((tr) => tr.effects.some((e) => e.is(setReviewComments))),
  })

  const deletedSelectionHandler = EditorView.domEventHandlers({
    mouseup(_event, view) {
      const hit = deletedSelectionAnchor(view)
      if (hit) {
        cb.onSelectionChange(hit.anchor, { x: hit.rect.left, y: hit.rect.bottom + 6 })
      }
    },
  })

  return [
    commentsState,
    commentDecorations,
    commentGutterClasses,
    commentBarGutter,
    addCommentGutter,
    selectionPopupListener(cb),
    deletedSelectionHandler,
    deletedChunkHighlighter(commentsState),
  ]
}
