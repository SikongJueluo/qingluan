# CLI review 入口：daemon 宿主 + 薄 CLI + 会话态评论

为人机不同速度协作（agent 连续产出多个 commits，人攒一批一起审），
review 入口设计为：**CLI 只做通知，daemon 做全部实际工作**。

- `qingluan review <dir> [--from <rev>] [--to <rev>]`：薄入口，POST 到
  daemon 建会话并输出 URL（首行纯 URL，`--open` 唤起浏览器，`--json`
  给 agent）。不嵌 UI、不算 diff。
- **前端**从 `apps/desktop/frontend` 抽为 `apps/web`，Tauri 与 daemon
  共用构建产物；daemon 用 `include_dir!` 把产物编进二进制，serve
  `/review/<id>` 页面。
- **diff 在 daemon 侧**用 `jj diff --from <rev> --to <rev>` 子进程计算
  （默认 `main..@`）；纯 git 目录拒绝，提示先进 workspace。
- **评论不持久化**：这是一次性交接物（给人审 → 导出给 agent 改 → 扔），
  只存 daemon 内存。会话每次调用新建（不复用、无历史列表），daemon
  重启即清。SQLite 基建（qingluan-storage）刻意不用。
- **API 走 REST**（浏览器 fetch 限制 + daemon 现有风格）：两级加载——
  `GET /reviews/<id>/files` 元信息列表，`GET .../files/<n>?side=old|new`
  按需全文；评论 `GET/POST /reviews/<id>/comments`；导出 =
  `qingluan review export <id>`（markdown）+ 前端「复制为 Markdown」。

**重新评估的时机**：评论需要跨 daemon 重启保留、或出现多人协作 review
（不再是"给 agent 的一次性交接"）时，重新考虑评论持久化与会话历史。
