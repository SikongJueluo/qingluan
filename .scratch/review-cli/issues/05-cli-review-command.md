# 05 · CLI review 子命令与导出

Status: ready-for-agent
Blocked by: 02, 04

- `qingluan review <dir> [--from <rev>] [--to <rev>] [--open] [--json]`：
  POST 建会话，stdout 首行纯 URL（agent 友好），次行人话；`--open`
  唤起系统浏览器。
- `qingluan review export <id>`：输出 markdown（文件路径 + 行范围 +
  side + 评论内容），即 agent 交接物格式。
- 前端补「复制为 Markdown」按钮（同 export 格式，纯前端拼字符串）。

参考：`docs/adr/0003-review-cli-architecture.md`。
