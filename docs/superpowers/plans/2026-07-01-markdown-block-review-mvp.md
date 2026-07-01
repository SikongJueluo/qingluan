# Markdown Block Review MVP Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a Vue 3 Markdown block review MVP that renders `MarkdownBlock[]` as selectable, commentable, Agent-feedback-ready blocks with a demo route.

**Architecture:** Add a small block-rendering subsystem under `frontend/src/components/markdown` with `BlockFrame` owning interaction and individual block components owning display only. Add shared block selection state via `useBlockSelection`, then wire a `/markdown-review` demo page using mock data and a sidebar.

**Tech Stack:** Vue 3 `<script setup>`, TypeScript, Tailwind CSS v4, shadcn-vue/Reka UI, `lucide-vue-next`, Vitest, Vue Test Utils, Jujutsu.

---

## File map

- Create `frontend/src/markdown/types.ts` — shared Markdown block and comment draft types.
- Create `frontend/src/composables/useBlockSelection.ts` — shared selection state and operations.
- Create `frontend/src/components/markdown/InlineText.vue` — plain text inline renderer placeholder.
- Create `frontend/src/components/markdown/BlockToolbar.vue` — hover toolbar actions and block type badge.
- Create `frontend/src/components/markdown/BlockFrame.vue` — block shell, hover state, selection, event forwarding.
- Create `frontend/src/components/markdown/MarkdownDocument.vue` — block list traversal and renderer dispatch.
- Create `frontend/src/components/markdown/MarkdownReviewLayout.vue` — resizable two-column review layout.
- Create `frontend/src/components/markdown/blocks/*.vue` — focused renderers for supported block types.
- Create `frontend/src/views/MarkdownReviewDemoView.vue` — demo route with mock blocks and sidebar state.
- Modify `frontend/src/router/index.ts` — add `/markdown-review` route.
- Create `frontend/src/__tests__/useBlockSelection.spec.ts` — composable behavior tests.
- Create `frontend/src/__tests__/MarkdownDocument.spec.ts` — renderer, event, and safe rendering tests.
- Modify `frontend/package.json` and `frontend/bun.lock` only through the shadcn-vue component install command.

## Task 1: Install missing shadcn-vue components

**Files:**
- Modify: `frontend/package.json`
- Modify: `frontend/bun.lock`
- Create: `frontend/src/components/ui/badge/*`
- Create: `frontend/src/components/ui/dropdown-menu/*`
- Create: `frontend/src/components/ui/popover/*`
- Create: `frontend/src/components/ui/scroll-area/*`
- Create: `frontend/src/components/ui/resizable/*`
- Create: `frontend/src/components/ui/table/*`
- Create: `frontend/src/components/ui/textarea/*`

- [ ] **Step 1: Confirm current Jujutsu state and parent changes**

Run from repository root:

```bash
jj st
```

Expected: working copy is on the implementation commit, with only changes from this feature in `@`. The parent may contain pre-existing `.justfile`, `frontend/bun.lock`, and `frontend/package.json` changes.

- [ ] **Step 2: Install the required UI components**

Run from `frontend/`:

```bash
bunx --bun shadcn-vue@latest add badge dropdown-menu popover scroll-area resizable table textarea
```

Expected: shadcn-vue creates the missing component folders under `frontend/src/components/ui/` and updates Bun metadata without creating a pnpm lockfile.

- [ ] **Step 3: Verify barrel exports exist**

Run from repository root:

```bash
jj --no-pager diff --git -- frontend/src/components/ui
```

Expected: each new component directory has an `index.ts` that exports PascalCase Vue components such as `Badge`, `DropdownMenu`, `ScrollArea`, `ResizablePanelGroup`, `Table`, and `Textarea`.

- [ ] **Step 4: Run a first type check after component generation**

Run from `frontend/`:

```bash
bun run type-check
```

Expected: type check may still fail because implementation files do not exist yet, but generated shadcn-vue component files should not introduce type errors by themselves.

- [ ] **Step 5: Commit checkpoint**

Run from repository root:

```bash
jj st
jj --no-pager diff --git -- frontend/src/components/ui frontend/package.json frontend/bun.lock
```

Expected: the diff contains only shadcn-vue generated component files and package/lock updates.

## Task 2: Add shared Markdown block types

**Files:**
- Create: `frontend/src/markdown/types.ts`

- [ ] **Step 1: Create the type file**

Write `frontend/src/markdown/types.ts`:

```ts
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
```

- [ ] **Step 2: Run type check**

Run from `frontend/`:

```bash
bun run type-check
```

Expected: this file has no type errors.

- [ ] **Step 3: Commit checkpoint**

Run from repository root:

```bash
jj st
jj --no-pager diff --git -- frontend/src/markdown/types.ts
```

Expected: diff contains only the new type definitions.

## Task 3: Add block selection composable with tests

**Files:**
- Create: `frontend/src/composables/useBlockSelection.ts`
- Create: `frontend/src/__tests__/useBlockSelection.spec.ts`

- [ ] **Step 1: Write the failing composable test**

Create `frontend/src/__tests__/useBlockSelection.spec.ts`:

```ts
import { beforeEach, describe, expect, it } from 'vitest'
import { useBlockSelection } from '@/composables/useBlockSelection'

describe('useBlockSelection', () => {
  beforeEach(() => {
    useBlockSelection().clearSelection()
  })

  it('selects a single block and stores it as the anchor', () => {
    const selection = useBlockSelection()

    selection.selectBlock('block-a')

    expect([...selection.selectedBlockIds.value]).toEqual(['block-a'])
    expect(selection.anchorBlockId.value).toBe('block-a')
  })

  it('toggles additive selections', () => {
    const selection = useBlockSelection()

    selection.selectBlock('block-a')
    selection.selectBlock('block-b', { additive: true })
    expect(selection.selectedBlockIds.value.has('block-a')).toBe(true)
    expect(selection.selectedBlockIds.value.has('block-b')).toBe(true)

    selection.selectBlock('block-b', { additive: true })
    expect([...selection.selectedBlockIds.value]).toEqual(['block-a'])
  })

  it('keeps anchor and current block for range selections', () => {
    const selection = useBlockSelection()

    selection.selectBlock('block-a')
    selection.selectBlock('block-c', { range: true })

    expect(selection.selectedBlockIds.value.has('block-a')).toBe(true)
    expect(selection.selectedBlockIds.value.has('block-c')).toBe(true)
    expect(selection.anchorBlockId.value).toBe('block-a')
  })

  it('clears selection and anchor', () => {
    const selection = useBlockSelection()

    selection.selectBlock('block-a')
    selection.clearSelection()

    expect([...selection.selectedBlockIds.value]).toEqual([])
    expect(selection.anchorBlockId.value).toBeNull()
  })
})
```

- [ ] **Step 2: Run the test to verify it fails**

Run from `frontend/`:

```bash
bun run test:unit -- src/__tests__/useBlockSelection.spec.ts
```

Expected: FAIL because `@/composables/useBlockSelection` does not exist.

- [ ] **Step 3: Implement the composable**

Create `frontend/src/composables/useBlockSelection.ts`:

```ts
import { computed, ref } from 'vue'

const selected = ref<Set<string>>(new Set())
const anchorBlockId = ref<string | null>(null)

export function useBlockSelection() {
  function selectBlock(
    blockId: string,
    options?: {
      range?: boolean
      additive?: boolean
    },
  ) {
    if (!options?.additive && !options?.range) {
      selected.value = new Set([blockId])
      anchorBlockId.value = blockId
      return
    }

    if (options?.additive) {
      const next = new Set(selected.value)
      if (next.has(blockId)) next.delete(blockId)
      else next.add(blockId)
      selected.value = next
      anchorBlockId.value = blockId
      return
    }

    selected.value = new Set([anchorBlockId.value ?? blockId, blockId])
  }

  function clearSelection() {
    selected.value = new Set()
    anchorBlockId.value = null
  }

  return {
    selectedBlockIds: computed(() => selected.value),
    anchorBlockId: computed(() => anchorBlockId.value),
    selectBlock,
    clearSelection,
  }
}
```

- [ ] **Step 4: Run the composable test**

Run from `frontend/`:

```bash
bun run test:unit -- src/__tests__/useBlockSelection.spec.ts
```

Expected: PASS for all four tests.

- [ ] **Step 5: Commit checkpoint**

Run from repository root:

```bash
jj st
jj --no-pager diff --git -- frontend/src/composables/useBlockSelection.ts frontend/src/__tests__/useBlockSelection.spec.ts
```

Expected: diff contains only the composable and its test.

## Task 4: Add focused block renderer components

**Files:**
- Create: `frontend/src/components/markdown/InlineText.vue`
- Create: `frontend/src/components/markdown/blocks/HeadingBlock.vue`
- Create: `frontend/src/components/markdown/blocks/ParagraphBlock.vue`
- Create: `frontend/src/components/markdown/blocks/CodeBlock.vue`
- Create: `frontend/src/components/markdown/blocks/ListBlock.vue`
- Create: `frontend/src/components/markdown/blocks/TableBlock.vue`
- Create: `frontend/src/components/markdown/blocks/BlockquoteBlock.vue`
- Create: `frontend/src/components/markdown/blocks/ImageBlock.vue`
- Create: `frontend/src/components/markdown/blocks/HrBlock.vue`
- Create: `frontend/src/components/markdown/blocks/UnknownBlock.vue`

- [ ] **Step 1: Create `InlineText.vue`**

Write `frontend/src/components/markdown/InlineText.vue`:

```vue
<script setup lang="ts">
defineProps<{
  text?: string
}>()
</script>

<template>
  <span>{{ text }}</span>
</template>
```

- [ ] **Step 2: Create heading and paragraph renderers**

Write `frontend/src/components/markdown/blocks/HeadingBlock.vue`:

```vue
<script setup lang="ts">
import { computed } from 'vue'
import type { MarkdownBlock } from '@/markdown/types'
import InlineText from '../InlineText.vue'

const props = defineProps<{
  block: MarkdownBlock
}>()

const tag = computed(() => {
  const depth = Math.min(Math.max(props.block.depth ?? 1, 1), 6)
  return `h${depth}`
})
</script>

<template>
  <component
    :is="tag"
    class="scroll-m-20 font-semibold tracking-tight"
    :class="{
      'mt-8 mb-4 text-3xl': block.depth === 1,
      'mt-7 mb-3 text-2xl': block.depth === 2,
      'mt-6 mb-2 text-xl': block.depth === 3,
      'mt-5 mb-2 text-lg': (block.depth ?? 1) >= 4,
    }"
  >
    <InlineText :text="block.text" />
  </component>
</template>
```

Write `frontend/src/components/markdown/blocks/ParagraphBlock.vue`:

```vue
<script setup lang="ts">
import type { MarkdownBlock } from '@/markdown/types'
import InlineText from '../InlineText.vue'

defineProps<{
  block: MarkdownBlock
}>()
</script>

<template>
  <p class="leading-7 text-foreground">
    <InlineText :text="block.text" />
  </p>
</template>
```

- [ ] **Step 3: Create code and table renderers**

Write `frontend/src/components/markdown/blocks/CodeBlock.vue`:

```vue
<script setup lang="ts">
import { Badge } from '@/components/ui/badge'
import type { MarkdownBlock } from '@/markdown/types'

defineProps<{
  block: MarkdownBlock
}>()
</script>

<template>
  <div class="overflow-hidden rounded-lg border bg-muted/40">
    <div class="flex items-center justify-between border-b px-3 py-1.5">
      <Badge variant="secondary">
        {{ block.lang || 'text' }}
      </Badge>
    </div>

    <pre class="overflow-x-auto p-4 text-sm leading-6"><code>{{ block.raw ?? block.text }}</code></pre>
  </div>
</template>
```

Write `frontend/src/components/markdown/blocks/TableBlock.vue`:

```vue
<script setup lang="ts">
import { computed } from 'vue'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import type { MarkdownBlock, MarkdownTableMeta } from '@/markdown/types'

const props = defineProps<{
  block: MarkdownBlock
}>()

const table = computed(() => props.block.meta?.table as MarkdownTableMeta | undefined)
</script>

<template>
  <div class="overflow-hidden rounded-md border">
    <Table v-if="table">
      <TableHeader>
        <TableRow>
          <TableHead v-for="(header, headerIndex) in table.headers" :key="`${header}-${headerIndex}`">
            {{ header }}
          </TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        <TableRow v-for="(row, rowIndex) in table.rows" :key="rowIndex">
          <TableCell v-for="(cell, cellIndex) in row" :key="cellIndex">
            {{ cell }}
          </TableCell>
        </TableRow>
      </TableBody>
    </Table>

    <pre v-else class="p-4 text-sm">{{ block.raw }}</pre>
  </div>
</template>
```

- [ ] **Step 4: Create list, quote, image, hr, and unknown renderers**

Write `frontend/src/components/markdown/blocks/ListBlock.vue`:

```vue
<script setup lang="ts">
import { computed } from 'vue'
import type { MarkdownBlock } from '@/markdown/types'
import InlineText from '../InlineText.vue'

const props = defineProps<{
  block: MarkdownBlock
}>()

const ordered = computed(() => props.block.meta?.ordered === true || props.block.meta?.listType === 'ordered')
const tag = computed(() => (ordered.value ? 'ol' : 'ul'))
const items = computed(() => props.block.children ?? [])
</script>

<template>
  <component :is="tag" class="my-2 space-y-1 pl-6" :class="ordered ? 'list-decimal' : 'list-disc'">
    <li v-for="item in items" :key="item.id" class="leading-7">
      <InlineText :text="item.text ?? item.raw" />
    </li>
    <li v-if="items.length === 0" class="leading-7">
      <InlineText :text="block.text ?? block.raw" />
    </li>
  </component>
</template>
```

Write `frontend/src/components/markdown/blocks/BlockquoteBlock.vue`:

```vue
<script setup lang="ts">
import type { MarkdownBlock } from '@/markdown/types'
import InlineText from '../InlineText.vue'

defineProps<{
  block: MarkdownBlock
}>()
</script>

<template>
  <blockquote class="border-l-4 border-muted-foreground/30 pl-4 italic text-muted-foreground">
    <InlineText :text="block.text ?? block.raw" />
  </blockquote>
</template>
```

Write `frontend/src/components/markdown/blocks/ImageBlock.vue`:

```vue
<script setup lang="ts">
import { computed } from 'vue'
import type { MarkdownBlock } from '@/markdown/types'

const props = defineProps<{
  block: MarkdownBlock
}>()

const src = computed(() => props.block.meta?.src as string | undefined)
const alt = computed(() => (props.block.meta?.alt as string | undefined) ?? props.block.text ?? '')
</script>

<template>
  <figure class="space-y-2">
    <img v-if="src" :src="src" :alt="alt" class="max-h-[420px] rounded-lg border object-contain" />
    <div v-else class="rounded-lg border border-dashed bg-muted/30 p-4 text-sm text-muted-foreground">
      图片地址缺失：{{ alt || block.raw || block.id }}
    </div>
    <figcaption v-if="alt" class="text-xs text-muted-foreground">
      {{ alt }}
    </figcaption>
  </figure>
</template>
```

Write `frontend/src/components/markdown/blocks/HrBlock.vue`:

```vue
<template>
  <div class="py-3">
    <div class="h-px bg-border" />
  </div>
</template>
```

Write `frontend/src/components/markdown/blocks/UnknownBlock.vue`:

```vue
<script setup lang="ts">
import { Badge } from '@/components/ui/badge'
import type { MarkdownBlock } from '@/markdown/types'

defineProps<{
  block: MarkdownBlock
}>()
</script>

<template>
  <div class="rounded-md border border-dashed bg-muted/20 p-3 text-sm">
    <div class="mb-2 flex items-center gap-2 text-muted-foreground">
      <Badge variant="secondary">{{ block.type }}</Badge>
      <span>未识别的 Markdown block</span>
    </div>
    <pre class="whitespace-pre-wrap text-foreground">{{ block.raw ?? block.text ?? block.id }}</pre>
  </div>
</template>
```

- [ ] **Step 5: Run type check**

Run from `frontend/`:

```bash
bun run type-check
```

Expected: renderer components compile after shadcn UI components are installed.

- [ ] **Step 6: Commit checkpoint**

Run from repository root:

```bash
jj st
jj --no-pager diff --git -- frontend/src/components/markdown
```

Expected: diff contains only the markdown renderer components created in this task.

## Task 5: Add toolbar, frame, document, and layout components with tests

**Files:**
- Create: `frontend/src/components/markdown/BlockToolbar.vue`
- Create: `frontend/src/components/markdown/BlockFrame.vue`
- Create: `frontend/src/components/markdown/MarkdownDocument.vue`
- Create: `frontend/src/components/markdown/MarkdownReviewLayout.vue`
- Create: `frontend/src/__tests__/MarkdownDocument.spec.ts`

- [ ] **Step 1: Write component tests first**

Create `frontend/src/__tests__/MarkdownDocument.spec.ts`:

```ts
import { describe, expect, it } from 'vitest'
import { mount } from '@vue/test-utils'
import MarkdownDocument from '@/components/markdown/MarkdownDocument.vue'
import { useBlockSelection } from '@/composables/useBlockSelection'
import type { MarkdownBlock } from '@/markdown/types'

const blocks: MarkdownBlock[] = [
  { id: 'heading-1', type: 'heading', order: 1, depth: 1, text: 'Title' },
  { id: 'paragraph-1', type: 'paragraph', order: 2, text: '<script>alert("x")</script>' },
  { id: 'code-1', type: 'code', order: 3, lang: 'ts', raw: 'const value = 1' },
  {
    id: 'table-1',
    type: 'table',
    order: 4,
    meta: { table: { headers: ['Name'], rows: [['Qingluan']] } },
  },
]

describe('MarkdownDocument', () => {
  it('renders each block with data-block-id and text content', () => {
    const wrapper = mount(MarkdownDocument, { props: { blocks } })

    expect(wrapper.find('[data-block-id="heading-1"]').exists()).toBe(true)
    expect(wrapper.find('[data-block-id="paragraph-1"]').exists()).toBe(true)
    expect(wrapper.text()).toContain('Title')
    expect(wrapper.text()).toContain('const value = 1')
    expect(wrapper.text()).toContain('Qingluan')
  })

  it('renders unsafe text as text instead of html', () => {
    const wrapper = mount(MarkdownDocument, { props: { blocks } })

    expect(wrapper.find('script').exists()).toBe(false)
    expect(wrapper.text()).toContain('<script>alert("x")</script>')
  })

  it('selects a block when its frame is clicked', async () => {
    useBlockSelection().clearSelection()
    const wrapper = mount(MarkdownDocument, { props: { blocks } })

    await wrapper.find('[data-block-id="heading-1"]').trigger('click')

    expect(useBlockSelection().selectedBlockIds.value.has('heading-1')).toBe(true)
  })

  it('emits comment and feedback events from toolbar actions', async () => {
    const wrapper = mount(MarkdownDocument, { props: { blocks } })

    await wrapper.find('[data-testid="comment-heading-1"]').trigger('click')
    await wrapper.find('[data-testid="feedback-heading-1"]').trigger('click')

    expect(wrapper.emitted('comment')?.[0]).toEqual(['heading-1'])
    expect(wrapper.emitted('feedback')?.[0]).toEqual(['heading-1'])
  })
})
```

- [ ] **Step 2: Run the component test to verify it fails**

Run from `frontend/`:

```bash
bun run test:unit -- src/__tests__/MarkdownDocument.spec.ts
```

Expected: FAIL because `MarkdownDocument.vue` and related components do not exist.

- [ ] **Step 3: Create `BlockToolbar.vue`**

Write `frontend/src/components/markdown/BlockToolbar.vue`:

```vue
<script setup lang="ts">
import { Bot, MessageSquare, MoreHorizontal } from 'lucide-vue-next'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from '@/components/ui/tooltip'
import type { MarkdownBlock } from '@/markdown/types'

defineProps<{
  block: MarkdownBlock
}>()

const emit = defineEmits<{
  comment: []
  feedback: []
  copyText: []
  copyRaw: []
}>()
</script>

<template>
  <TooltipProvider>
    <div class="items-center gap-1 rounded-md border bg-background p-1 shadow-sm">
      <Tooltip>
        <TooltipTrigger as-child>
          <Button
            variant="ghost"
            size="icon-sm"
            :data-testid="`comment-${block.id}`"
            @click.stop="emit('comment')"
          >
            <MessageSquare class="size-4" />
          </Button>
        </TooltipTrigger>
        <TooltipContent>评论此块</TooltipContent>
      </Tooltip>

      <Tooltip>
        <TooltipTrigger as-child>
          <Button
            variant="ghost"
            size="icon-sm"
            :data-testid="`feedback-${block.id}`"
            @click.stop="emit('feedback')"
          >
            <Bot class="size-4" />
          </Button>
        </TooltipTrigger>
        <TooltipContent>反馈给 Agent</TooltipContent>
      </Tooltip>

      <DropdownMenu>
        <DropdownMenuTrigger as-child>
          <Button variant="ghost" size="icon-sm" @click.stop>
            <MoreHorizontal class="size-4" />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="start">
          <DropdownMenuItem @click="emit('copyText')">
            复制纯文本
          </DropdownMenuItem>
          <DropdownMenuItem @click="emit('copyRaw')">
            复制 Markdown 原文
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>

      <Badge variant="secondary" class="ml-1 text-[10px]">
        {{ block.type }}
      </Badge>
    </div>
  </TooltipProvider>
</template>
```

- [ ] **Step 4: Create `BlockFrame.vue`**

Write `frontend/src/components/markdown/BlockFrame.vue`:

```vue
<script setup lang="ts">
import { computed } from 'vue'
import { useBlockSelection } from '@/composables/useBlockSelection'
import type { MarkdownBlock } from '@/markdown/types'
import BlockToolbar from './BlockToolbar.vue'

const props = defineProps<{
  block: MarkdownBlock
}>()

const emit = defineEmits<{
  comment: [blockId: string]
  feedback: [blockId: string]
}>()

const selection = useBlockSelection()
const selected = computed(() => selection.selectedBlockIds.value.has(props.block.id))

function handleClick(event: MouseEvent) {
  selection.selectBlock(props.block.id, {
    range: event.shiftKey,
  })
}
</script>

<template>
  <div
    :data-block-id="block.id"
    class="group relative rounded-md px-3 py-1.5 transition-colors"
    :class="selected ? 'bg-primary/10 ring-1 ring-primary/30' : 'hover:bg-muted/50'"
    @click="handleClick"
  >
    <BlockToolbar
      class="absolute -left-10 top-1.5 hidden group-hover:flex"
      :block="block"
      @comment="emit('comment', block.id)"
      @feedback="emit('feedback', block.id)"
    />

    <slot />
  </div>
</template>
```

- [ ] **Step 5: Create `MarkdownDocument.vue`**

Write `frontend/src/components/markdown/MarkdownDocument.vue`:

```vue
<script setup lang="ts">
import type { Component } from 'vue'
import type { MarkdownBlock } from '@/markdown/types'
import BlockFrame from './BlockFrame.vue'
import BlockquoteBlock from './blocks/BlockquoteBlock.vue'
import CodeBlock from './blocks/CodeBlock.vue'
import HeadingBlock from './blocks/HeadingBlock.vue'
import HrBlock from './blocks/HrBlock.vue'
import ImageBlock from './blocks/ImageBlock.vue'
import ListBlock from './blocks/ListBlock.vue'
import ParagraphBlock from './blocks/ParagraphBlock.vue'
import TableBlock from './blocks/TableBlock.vue'
import UnknownBlock from './blocks/UnknownBlock.vue'

defineProps<{
  blocks: MarkdownBlock[]
}>()

const emit = defineEmits<{
  comment: [blockId: string]
  feedback: [blockId: string]
}>()

const renderers: Partial<Record<MarkdownBlock['type'], Component>> = {
  heading: HeadingBlock,
  paragraph: ParagraphBlock,
  code: CodeBlock,
  list: ListBlock,
  table: TableBlock,
  blockquote: BlockquoteBlock,
  image: ImageBlock,
  hr: HrBlock,
}

function getRenderer(block: MarkdownBlock): Component {
  return renderers[block.type] ?? UnknownBlock
}
</script>

<template>
  <article class="mx-auto max-w-4xl px-8 py-10">
    <BlockFrame
      v-for="block in blocks"
      :key="block.id"
      :block="block"
      @comment="emit('comment', $event)"
      @feedback="emit('feedback', $event)"
    >
      <component :is="getRenderer(block)" :block="block" />
    </BlockFrame>
  </article>
</template>
```

- [ ] **Step 6: Create `MarkdownReviewLayout.vue`**

Write `frontend/src/components/markdown/MarkdownReviewLayout.vue`:

```vue
<script setup lang="ts">
import {
  ResizableHandle,
  ResizablePanel,
  ResizablePanelGroup,
} from '@/components/ui/resizable'
import { ScrollArea } from '@/components/ui/scroll-area'
import type { MarkdownBlock } from '@/markdown/types'
import MarkdownDocument from './MarkdownDocument.vue'

defineProps<{
  blocks: MarkdownBlock[]
}>()

const emit = defineEmits<{
  comment: [blockId: string]
  feedback: [blockId: string]
}>()
</script>

<template>
  <ResizablePanelGroup direction="horizontal" class="h-full min-h-[calc(100vh-4rem)] w-full">
    <ResizablePanel :default-size="70" :min-size="45">
      <ScrollArea class="h-full">
        <MarkdownDocument
          :blocks="blocks"
          @comment="emit('comment', $event)"
          @feedback="emit('feedback', $event)"
        />
      </ScrollArea>
    </ResizablePanel>

    <ResizableHandle />

    <ResizablePanel :default-size="30" :min-size="20">
      <div class="h-full border-l bg-muted/20 p-4">
        <slot name="sidebar">
          <div class="text-sm text-muted-foreground">
            暂无评论
          </div>
        </slot>
      </div>
    </ResizablePanel>
  </ResizablePanelGroup>
</template>
```

- [ ] **Step 7: Run component tests and type check**

Run from `frontend/`:

```bash
bun run test:unit -- src/__tests__/MarkdownDocument.spec.ts
bun run type-check
```

Expected: MarkdownDocument tests pass and type check succeeds for markdown components.

- [ ] **Step 8: Commit checkpoint**

Run from repository root:

```bash
jj st
jj --no-pager diff --git -- frontend/src/components/markdown frontend/src/__tests__/MarkdownDocument.spec.ts
```

Expected: diff contains toolbar, frame, document, layout, and component test changes.

## Task 6: Add demo route and sidebar behavior

**Files:**
- Create: `frontend/src/views/MarkdownReviewDemoView.vue`
- Modify: `frontend/src/router/index.ts`

- [ ] **Step 1: Create the demo view**

Write `frontend/src/views/MarkdownReviewDemoView.vue`:

```vue
<script setup lang="ts">
import { computed, ref } from 'vue'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Separator } from '@/components/ui/separator'
import { Textarea } from '@/components/ui/textarea'
import MarkdownReviewLayout from '@/components/markdown/MarkdownReviewLayout.vue'
import type { BlockCommentDraft, MarkdownBlock } from '@/markdown/types'

const blocks: MarkdownBlock[] = [
  { id: 'block-1', type: 'heading', order: 1, depth: 1, text: 'Markdown Block 审查组件 MVP' },
  {
    id: 'block-2',
    type: 'paragraph',
    order: 2,
    text: '这是一个 Notion 风格的 Markdown 阅读审查界面示例。每个段落、标题、代码块和表格都是独立 block。',
  },
  { id: 'block-3', type: 'blockquote', order: 3, text: '审查界面应该突出内容，同时让评论和 Agent 反馈入口保持轻量。' },
  {
    id: 'block-4',
    type: 'code',
    order: 4,
    lang: 'ts',
    raw: 'const selected = new Set<string>()\nselected.add("block-1")',
  },
  {
    id: 'block-5',
    type: 'list',
    order: 5,
    meta: { ordered: false },
    children: [
      { id: 'block-5-1', type: 'listItem', order: 1, text: 'hover 显示 block 工具条' },
      { id: 'block-5-2', type: 'listItem', order: 2, text: '点击 block 显示选中态' },
      { id: 'block-5-3', type: 'listItem', order: 3, text: '按钮向父组件发出事件' },
    ],
  },
  {
    id: 'block-6',
    type: 'table',
    order: 6,
    meta: {
      table: {
        headers: ['Block', '职责'],
        rows: [
          ['BlockFrame', '交互壳层'],
          ['MarkdownDocument', '分发渲染'],
          ['MarkdownReviewLayout', '审查布局'],
        ],
      },
    },
  },
  { id: 'block-7', type: 'image', order: 7, text: '示例图片占位', meta: { alt: '未提供图片地址' } },
  { id: 'block-8', type: 'hr', order: 8 },
  { id: 'block-9', type: 'unknown', order: 9, raw: '未来解析器输出的新 block 类型会先降级显示。' },
]

const activeBlockId = ref<string | null>(null)
const activeMode = ref<'comment' | 'feedback' | null>(null)
const draft = ref<BlockCommentDraft>({ blockId: '', content: '' })

const activeBlock = computed(() => blocks.find((block) => block.id === activeBlockId.value))

function handleComment(blockId: string) {
  activeBlockId.value = blockId
  activeMode.value = 'comment'
  draft.value = { blockId, content: '' }
}

function handleFeedback(blockId: string) {
  activeBlockId.value = blockId
  activeMode.value = 'feedback'
  draft.value = { blockId, content: '' }
}

function clearDraft() {
  activeBlockId.value = null
  activeMode.value = null
  draft.value = { blockId: '', content: '' }
}
</script>

<template>
  <div class="h-full min-h-[calc(100vh-4rem)]">
    <MarkdownReviewLayout :blocks="blocks" @comment="handleComment" @feedback="handleFeedback">
      <template #sidebar>
        <div class="space-y-4">
          <div>
            <h2 class="text-sm font-semibold">审查侧栏</h2>
            <p class="mt-1 text-sm text-muted-foreground">
              选择 block 后，可在这里编写评论或准备给 Agent 的反馈。
            </p>
          </div>

          <Separator />

          <div v-if="activeBlock" class="space-y-3">
            <div class="flex items-center gap-2">
              <Badge variant="secondary">{{ activeMode === 'feedback' ? 'Agent 反馈' : '评论' }}</Badge>
              <span class="text-xs text-muted-foreground">{{ activeBlock.id }}</span>
            </div>

            <div class="rounded-md border bg-background p-3 text-sm">
              <div class="mb-1 font-medium">{{ activeBlock.type }}</div>
              <div class="line-clamp-3 text-muted-foreground">
                {{ activeBlock.text ?? activeBlock.raw ?? activeBlock.id }}
              </div>
            </div>

            <Textarea v-model="draft.content" placeholder="输入快速评论或 Agent 反馈说明" />

            <div class="flex justify-end gap-2">
              <Button variant="ghost" size="sm" @click="clearDraft">
                清空
              </Button>
              <Button size="sm" disabled>
                MVP 暂不提交
              </Button>
            </div>
          </div>

          <div v-else class="rounded-md border border-dashed p-4 text-sm text-muted-foreground">
            暂无评论。将鼠标悬停在左侧 block 上，点击评论或 Agent 按钮开始。
          </div>
        </div>
      </template>
    </MarkdownReviewLayout>
  </div>
</template>
```

- [ ] **Step 2: Add the demo route**

Replace `frontend/src/router/index.ts` with:

```ts
import { createRouter, createWebHistory } from 'vue-router'
import HomeView from '@/views/HomeView.vue'
import MarkdownReviewDemoView from '@/views/MarkdownReviewDemoView.vue'

const router = createRouter({
  history: createWebHistory(import.meta.env.BASE_URL),
  routes: [
    {
      path: '/',
      name: 'home',
      component: HomeView,
    },
    {
      path: '/markdown-review',
      name: 'markdown-review',
      component: MarkdownReviewDemoView,
    },
  ],
})

export default router
```

- [ ] **Step 3: Run route-level type check**

Run from `frontend/`:

```bash
bun run type-check
```

Expected: demo view and router compile.

- [ ] **Step 4: Commit checkpoint**

Run from repository root:

```bash
jj st
jj --no-pager diff --git -- frontend/src/views/MarkdownReviewDemoView.vue frontend/src/router/index.ts
```

Expected: diff contains only the new demo view and route registration.

## Task 7: Final validation and polish

**Files:**
- Inspect: all files changed by Tasks 1-6
- Modify only if validation reveals a concrete issue.

- [ ] **Step 1: Run unit tests**

Run from `frontend/`:

```bash
bun run test:unit
```

Expected: all unit tests pass, including existing tests or any updated tests. If the existing `App.spec.ts` still expects old starter text, update it to assert the current shell renders instead of obsolete copy.

- [ ] **Step 2: Run type check**

Run from `frontend/`:

```bash
bun run type-check
```

Expected: PASS.

- [ ] **Step 3: Run production build**

Run from `frontend/`:

```bash
bun run build
```

Expected: PASS and Vite emits production assets.

- [ ] **Step 4: Manual browser smoke test**

Run from `frontend/`:

```bash
bun run dev
```

Open `/markdown-review` in the local dev server.

Expected:

- Mock document renders with heading, paragraph, quote, code, list, table, image fallback, divider, and unknown block.
- Hovering a block shows the left toolbar.
- Clicking a block applies selected styling.
- Clicking the comment button updates the sidebar to comment mode.
- Clicking the Agent button updates the sidebar to Agent feedback mode.
- The page does not execute the unsafe paragraph text from tests as HTML.

- [ ] **Step 5: Review final diff**

Run from repository root:

```bash
jj --no-pager diff --git
```

Expected: final diff contains only the Markdown review MVP, generated shadcn-vue components, test additions, route addition, and the approved spec/plan docs. Pre-existing parent changes remain in the parent commit and are not mixed into this commit.

- [ ] **Step 6: Final commit description check**

Run from repository root:

```bash
jj st
```

Expected: current working copy commit description is `Add markdown block review MVP spec` if only docs have been created, or update it before implementation completion to a feature message such as:

```bash
jj desc -m "Add markdown block review MVP"
```

Expected after description update: `jj st` shows the feature changes in the current Jujutsu commit with a clear description.

## Self-review

- Spec coverage: Tasks cover shadcn-vue component installation, type definitions, selection state, block renderers, interaction shell, toolbar, layout, demo route, tests, and validation.
- Placeholder scan: The plan uses concrete file paths, code, commands, and expected outcomes.
- Type consistency: All component props use `MarkdownBlock`; event names are `comment` and `feedback`; table metadata is `MarkdownTableMeta`; route path is `/markdown-review`.
