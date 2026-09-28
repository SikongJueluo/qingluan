/**
 * Mock markdown 审查文档（/markdown-review/:id 页面占位）。
 * 审批列表已接 daemon review 会话；此页面暂无真实入口，
 * markdown 类会话上线后由审查会话提供内容。
 */
import type { MarkdownBlock } from '@/markdown/types'

export const markdownDocs: Record<string, MarkdownBlock[]> = {
  'r-2402': [
    { id: 'b-1', type: 'heading', order: 1, depth: 1, text: 'ADR-008：terminal gRPC 只走 UDS' },
    { id: 'b-2', type: 'paragraph', order: 2, text: '状态：已接受（2026-04-24）' },
    {
      id: 'b-3',
      type: 'paragraph',
      order: 3,
      text: 'terminal 数据流曾评估 TCP + UDS 双通道。TCP 暴露面大，且局域网场景没有真实需求——桌面端与 daemon 同机，远程场景由 SSH 转发承担。',
    },
    { id: 'b-4', type: 'heading', order: 4, depth: 2, text: '决定' },
    {
      id: 'b-5',
      type: 'paragraph',
      order: 5,
      text: 'gRPC 终端通道固定使用私有 UDS（$XDG_RUNTIME_DIR/qingluan/terminal.sock），daemon 不提供 TCP 终端端口。',
    },
    {
      id: 'b-6',
      type: 'list',
      order: 6,
      meta: { ordered: true },
      children: [
        { id: 'b-6-1', type: 'listItem', order: 1, text: 'UDS 路径仅属主可读写' },
        {
          id: 'b-6-2',
          type: 'listItem',
          order: 2,
          text: '互操作测试走 tests/terminal-grpc-interop',
        },
        {
          id: 'b-6-3',
          type: 'listItem',
          order: 3,
          text: 'TS 客户端封装在 packages/qingluan-client',
        },
      ],
    },
    {
      id: 'b-7',
      type: 'blockquote',
      order: 7,
      text: '代价：非本机客户端必须自行解决传输（SSH 隧道或 mosh），不内置。',
    },
    {
      id: 'b-8',
      type: 'code',
      order: 8,
      lang: 'toml',
      raw: '[terminal]\ntransport = "uds"\npath = "${XDG_RUNTIME_DIR}/qingluan/terminal.sock"',
    },
  ],
  'r-2405': [
    { id: 'b-1', type: 'heading', order: 1, depth: 1, text: 'BlockFrame 交互壳层' },
    {
      id: 'b-2',
      type: 'paragraph',
      order: 2,
      text: 'BlockFrame 是所有 block 的交互容器：选中态、悬停工具条与评论锚点都由它提供，内容渲染则下放给各 block 组件。',
    },
    {
      id: 'b-3',
      type: 'table',
      order: 3,
      meta: {
        table: {
          headers: ['组件', '职责'],
          rows: [
            ['BlockFrame', '交互壳层（选中/工具条/评论锚点）'],
            ['MarkdownDocument', '按 order 分发渲染'],
            ['MarkdownReviewLayout', '审查布局与侧栏'],
          ],
        },
      },
    },
    { id: 'b-4', type: 'hr', order: 4 },
    {
      id: 'b-5',
      type: 'paragraph',
      order: 5,
      text: '> 示例代码与 API 表格待补。',
    },
    { id: 'b-6', type: 'unknown', order: 6, raw: '新解析器输出的 block 类型先降级显示。' },
  ],
  'r-2406': [
    { id: 'b-1', type: 'heading', order: 1, depth: 1, text: '场景图预训练数据清洗 · 周报' },
    {
      id: 'b-2',
      type: 'paragraph',
      order: 2,
      text: '本周清理了 3 组离线场景图数据，剔除 12% 姿态漂移样本。剔除标准：连续 5 帧以上位姿跳变超过阈值。',
    },
    { id: 'b-3', type: 'heading', order: 3, depth: 2, text: '结论' },
    {
      id: 'b-4',
      type: 'paragraph',
      order: 4,
      text: '清洗后 val SPL 提升 2.4，但嵌入式端推理耗时增加 8ms，待评估是否接受。',
    },
    {
      id: 'b-5',
      type: 'code',
      order: 5,
      lang: 'python',
      raw: 'ds = SceneGraphDataset(root="data/clean-v3")\nloader = DataLoader(ds, batch_size=16, num_workers=4)',
    },
    {
      id: 'b-6',
      type: 'image',
      order: 6,
      text: '清洗前后 SPL 对比图',
      meta: { alt: '实验结果图占位' },
    },
  ],
  'r-2408': [
    { id: 'b-1', type: 'heading', order: 1, depth: 1, text: 'subagent 委派规范 v2' },
    {
      id: 'b-2',
      type: 'paragraph',
      order: 2,
      text: 'v1 的边界讨论过长；v2 收敛为「先描述、再授权、后回收」三段式。',
    },
    { id: 'b-3', type: 'blockquote', order: 3, text: '开放问题：委派失败的回滚语义尚未定稿。' },
  ],
  'r-2397': [
    { id: 'b-1', type: 'heading', order: 1, depth: 1, text: 'README 快速上手' },
    {
      id: 'b-2',
      type: 'paragraph',
      order: 2,
      text: '安装、启动 daemon、workspace 管理与桌面端使用说明（定稿版）。',
    },
    {
      id: 'b-3',
      type: 'code',
      order: 3,
      lang: 'bash',
      raw: 'qingluan daemon start\nqingluan workspace list',
    },
  ],
}
