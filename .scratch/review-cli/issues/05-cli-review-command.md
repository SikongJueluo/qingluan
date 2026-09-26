# 05 · CLI review 子命令与导出

Status: resolved
Blocked by: 02, 04

- `qingluan review <dir> [--from <rev>] [--to <rev>] [--open] [--json]`：
  POST 建会话，stdout 首行纯 URL（agent 友好），次行人话；`--open`
  唤起系统浏览器。
- `qingluan review export <id>`：输出 markdown（文件路径 + 行范围 +
  side + 评论内容），即 agent 交接物格式。
- 前端补「复制为 Markdown」按钮（同 export 格式，纯前端拼字符串）。

参考：`docs/adr/0003-review-cli-architecture.md`。

## Comments

- 2026-09-26 agent：完成。`qingluan review <dir> [--from] [--to] [--open]
  [--json]`：POST 建会话，stdout 首行纯 URL（含 ?from=&to= 供前端显示），
  次行人话；`--open` 走 xdg-open；`--json` 输出 {ok,id,url}。
- `qingluan review export <id>`：评论按文件分组 → markdown
  （`# Review comments` / `## path` / `- [side Lf-Lt] author: content`，
  多行缩进两格）；`comments_markdown` 纯函数 + 单测。
- 前端「复制为 Markdown」按钮（CodeReviewView 头部，评论数为 0 时禁用），
  `review-export.ts` 与 CLI 同格式 + 单测（两侧格式测试互为镜像）。
- E2E：daemon 在跑时 review/export 输出如上；`just quality` 全绿。
