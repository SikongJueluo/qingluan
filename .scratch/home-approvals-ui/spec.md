# 主页与审批收件箱

## Status: resolved

## 问题

提交驱动（diff 审查 / markdown 审查 / 执行确认）的工具，主页应该是
「面板控制器 + mailbox」的形态。用 5 个结构化 UI 变体原型验证。

## 决定（2026-04，原型 E 胜出）

- **主页 `/` 只做概览**：可点击统计卡（跳 `/approvals?filter=…`）+
  近 7 日审批量 + 各 agent 待办 + agent 动态。不放处理动作，不造
  daemon 拿不到的指标（如平均响应时长）。
- **审批收件箱 `/approvals`**（sidebar「审批」）：单行卡片 + 两个按钮
  （同意 / 详情）。
  - 同意的语义随类型：diff → agent rebase 到主分支完成提交；
    confirm → agent 按建议继续；markdown → 定稿。
  - 详情 = 请求修改的入口：进完整审查页评论后反馈给 agent。
  - 分组：仓库（默认）/ 工作区 / 状态。
- diff 审查对象是 from..to 合并范围（多 commit），数据模型带
  `commits` / `range` / `commitList`。
- 首版数据源为 `src/approvals/mock-data.ts` 占位。

## 原型存档

完整 5 变体（A 三栏收件箱 / B 指挥面板 / C 聚焦分诊流 / D 融合工作台
已否决 / E 概览+审批拆分）见 bookmark **`prototype/home-ui`**，不进 main。

## 后续

- issues/01-connect-daemon-data.md
