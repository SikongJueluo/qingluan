# 接入 daemon 审批数据源

Status: needs-triage

## 问题

`/`（概览）与 `/approvals`（审批收件箱）目前用
`apps/web/src/approvals/mock-data.ts` 占位。需要接 daemon 的真实
review 会话/提交数据，并把「同意」动作回传 daemon（diff → 触发
agent rebase；confirm → 放行；markdown → 定稿）。

## 范围

- daemon 侧新增审批队列端点（列出提交、按类型/状态过滤、批准回执）
- confirm 类型目前 daemon 无对应概念，需要协议扩展
- 首页统计（7 日审批量、agent 待办分布）按会话时间戳聚合
- 详情按钮跳真实路由：diff → `/review/:id`，markdown → 文档审查页

## 参考

- 领域类型：`apps/web/src/approvals/types.ts`
- 决定背景：`.scratch/home-approvals-ui/spec.md`
