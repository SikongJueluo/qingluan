# 06 · review UI 复用复杂度引擎（第二阶段）

Status: ready-for-agent
Blocked by: 05

本期（v1）**不实现**，只记录接口边界，避免 01–05 把它焊死。

- daemon 侧对 `GET /reviews/<id>/files` 拿到的全文调 `qingluan-complexity`，
  按文件算一次、存在内存会话里（评论不持久化的既有设计不变）。
- 面向 review 的输出是增量视角：diff 中被改动函数的复杂度新值
  （「这次改动把 `classify` 的认知复杂度从 9 提到 15」），比绝对值有用。
- 超阈值在 review UI 标记，等价于 Sonar S3776/S1541 issue。
- 依赖：`qingluan-complexity` 的公开 API 必须是纯函数、无 IO 假设，
  路径由调用方给（CLI 给相对 root 的路径，daemon 给会话内路径）。

## Comments

（未开工）
