# Markdown Block 审查组件 MVP 设计说明

## 背景

本项目需要实现一个 Notion 风格的 Markdown 阅读审查界面。上层会传入结构化的 `MarkdownBlock[]`，前端将其渲染为多个独立 block，并支持 block 级 hover 工具条、单块选择、快速评论入口和反馈给 Agent 的入口。

本阶段不实现完整 Markdown 解析器，不处理 inline range comment，不接入评论持久化后端，也不让 Agent 自动修改文档。MVP 的核心是建立清晰、可扩展的组件边界，让后续 `unified + remark-parse + remark-gfm` 解析结果可以无痛映射到同一套 block 渲染界面。

## 调研结论

- 前端项目实际位于 `frontend/`。
- 项目使用 Vue 3 beta、TypeScript、Vite、Tailwind CSS v4、shadcn-vue 2.6.2、Reka UI。
- 当前已安装 shadcn-vue 组件包括 `button`、`input`、`separator`、`skeleton`、`tooltip`、`sheet`、`sidebar`。
- 本 MVP 需要补充安装 `badge`、`dropdown-menu`、`popover`、`scroll-area`、`resizable`、`table`、`textarea`。
- 当前项目已安装并使用 `lucide-vue-next`。虽然新资料显示 `@lucide/vue` 是更新推荐包名，但为避免扩大范围，MVP 先沿用项目现有依赖。
- 当前 `Button` 变体已支持 `size="icon-sm"`。
- `unified`、`remark-parse`、`remark-gfm`、`dompurify` 已在依赖中，但本阶段不使用它们构建解析器。
- 当前工作区在父提交中已有 `.justfile`、`frontend/bun.lock`、`frontend/package.json` 改动；MVP 实现必须避免覆盖或误归因这些既有改动。

## 方案选择

采用“组件 + 演示页面”方案。

相比只做纯组件，该方案能在浏览器中验证 hover 工具条、选中态、comment/feedback 事件和不同 block 的视觉表现。相比直接接入首页，该方案不会污染正式业务入口，也更适合 MVP 验收。

## 交付范围

### 包含

1. 新增 Markdown block 类型定义。
2. 新增 Markdown 审查布局组件、文档渲染组件、block 交互壳层、block 工具条和具体 block 渲染组件。
3. 新增 `useBlockSelection`，管理单块选择状态，并保留 Shift 范围选择接口。
4. 新增 `/markdown-review` 演示页面，用 mock `MarkdownBlock[]` 覆盖主要 block 类型。
5. 在演示页面右侧 sidebar 中展示最近的评论/Agent 反馈操作，并提供轻量评论输入框。

### 不包含

- 完整 Markdown 编辑器。
- Markdown 字符串到 AST/block 的解析器。
- inline range comment。
- 拖拽排序。
- 富文本编辑。
- Shiki 代码高亮。
- 虚拟滚动。
- 评论持久化后端。
- Agent 自动修改文档。
- `lucide-vue-next` 到 `@lucide/vue` 的迁移。

## 文件结构

新增文件：

```txt
frontend/src/markdown/types.ts

frontend/src/components/markdown/
  MarkdownReviewLayout.vue
  MarkdownDocument.vue
  BlockFrame.vue
  BlockToolbar.vue
  InlineText.vue
  blocks/
    HeadingBlock.vue
    ParagraphBlock.vue
    CodeBlock.vue
    ListBlock.vue
    TableBlock.vue
    BlockquoteBlock.vue
    ImageBlock.vue
    HrBlock.vue
    UnknownBlock.vue

frontend/src/composables/useBlockSelection.ts
frontend/src/views/MarkdownReviewDemoView.vue
```

修改文件：

```txt
frontend/src/router/index.ts
```

默认只添加 `/markdown-review` 路由，不修改现有首页和 sidebar 外观。若后续需要导航入口，可在单独变更中补充。

## 类型设计

`frontend/src/markdown/types.ts` 定义 block 协议：

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

export interface BlockCommentDraft {
  blockId: string
  content: string
}
```

补充约定：

- `meta.table` 可为 `{ headers: string[], rows: string[][] }`。
- `meta.ordered` 可为 `boolean`，供 `ListBlock` 判断有序/无序列表。
- `meta.src` / `meta.alt` 可供 `ImageBlock` 使用。

## 组件设计

### `MarkdownReviewLayout.vue`

负责整体审查布局。

- 接收 `blocks: MarkdownBlock[]`。
- 使用 `ResizablePanelGroup` 横向分栏。
- 左侧 panel 使用 `ScrollArea` 包裹 `MarkdownDocument`。
- 右侧 panel 提供评论 / Agent 反馈区域，默认显示空状态，支持 `sidebar` slot 覆盖。
- 向父层透传 `comment(blockId)` 与 `feedback(blockId)`。

### `MarkdownDocument.vue`

负责遍历 `MarkdownBlock[]` 并分发渲染组件。

- 外层使用 `<article>` 控制阅读宽度和间距。
- 每个 block 外层统一包 `BlockFrame`。
- 根据 `block.type` 映射到具体 block 组件。
- 未识别类型降级到 `UnknownBlock`。
- 不使用 `v-html`。
- 不使用 `Card` 包裹每个 block。

### `BlockFrame.vue`

负责 block 交互壳层。

- 暴露 `data-block-id`。
- hover 时显示左侧 `BlockToolbar`。
- 点击 block 时调用 `useBlockSelection().selectBlock(...)`。
- 选中态使用轻量背景和 ring 表达，不改变内容结构。
- 向上 emit `comment(blockId)`、`feedback(blockId)`。
- 不负责具体 Markdown 内容渲染。

### `BlockToolbar.vue`

负责 block 左侧操作条。

- 使用 `Button`、`Tooltip`、`DropdownMenu`、`Badge`。
- 提供评论按钮、反馈给 Agent 按钮、更多操作菜单。
- 更多操作菜单包括“复制纯文本”和“复制 Markdown 原文”。
- 图标沿用 `lucide-vue-next`。
- 所有按钮点击使用 `.stop`，避免触发 block 选择。

### 具体 block 组件

- `HeadingBlock`：根据 `depth` 渲染 `h1` 到 `h6`，并显式导入 `computed`。
- `ParagraphBlock`：渲染普通段落文本。
- `CodeBlock`：显示语言 badge 和 `pre/code`，不做语法高亮。
- `ListBlock`：根据 `meta.ordered` 或 `meta.listType` 渲染 `ol` / `ul`，children 渲染为 list item 文本。
- `TableBlock`：读取 `meta.table` 的二维数组结构；缺失时 fallback 到 `raw`。
- `BlockquoteBlock`：使用左边框和 muted 文本色。
- `ImageBlock`：读取 `meta.src` / `meta.alt` / `text`，缺失 src 时显示 fallback。
- `HrBlock`：渲染轻量分隔线。
- `UnknownBlock`：显示 block type 和 `raw/text` fallback，方便调试。
- `InlineText`：第一版只做安全纯文本输出占位，不解析 inline markdown。

## 选择状态设计

`useBlockSelection` 使用模块级 `ref` 保存选择状态，让所有 block 共享同一选择集合。

- 普通点击：清空旧选择，只选当前 block，并更新 anchor。
- additive 接口：预留多选能力，切换当前 block 是否选中。
- range 接口：第一版不实现完整连续顺序选择，只选择 anchor 与当前 block；后续可基于 `block.order` 实现连续范围选择。
- `clearSelection` 清空选择和 anchor。

## Demo 页面设计

新增 `MarkdownReviewDemoView.vue`。

- 页面路由为 `/markdown-review`。
- 内置 mock blocks，覆盖 heading、paragraph、blockquote、code、list、table、image、hr、unknown。
- 渲染 `MarkdownReviewLayout`。
- 监听 `comment` 与 `feedback`，在右侧 sidebar 显示最近操作。
- 使用 `Textarea` 提供评论草稿输入，但不持久化。

## 验收标准

1. `/markdown-review` 可以显示 mock `MarkdownBlock[]` 文档。
2. `MarkdownReviewLayout` 可以接收外部 `blocks` prop 渲染文档。
3. 每个 block hover 时显示左侧工具条。
4. 点击 block 后有选中态。
5. 点击评论按钮能向上 emit `comment(blockId)`。
6. 点击 Agent 按钮能向上 emit `feedback(blockId)`。
7. 标题、段落、代码块、表格、引用、列表具有明显不同样式。
8. 不使用 `v-html` 渲染未清洗内容。
9. 不把每个 block 包装成 `Card`。
10. 组件职责清晰：`BlockFrame` 管交互，具体 block 组件只管内容展示。
11. 后续可将 unified/remark 解析结果映射为 `MarkdownBlock[]` 后直接接入。

## 验证计划

实现后在 `frontend/` 下运行：

```bash
pnpm build
pnpm type-check
pnpm test:unit
```

如果当前环境实际以 bun lockfile 为准，执行安装或验证前需先确认包管理器，避免无意改写 lockfile。

## 风险与约束

- 安装 shadcn-vue 组件会修改 `frontend/package.json` 和 lockfile，需要保护已有工作区改动。
- 项目使用 Vue beta 与 TypeScript 6，组件 API 和类型检查必须以实际 `type-check` 为准。
- `html` block 第一版不渲染 HTML，只降级为纯文本或 unknown fallback，避免 XSS。
- 视觉风格必须保持轻量、Notion 风格，不使用 Card 堆叠造成视觉过重。
