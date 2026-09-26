import type { KanbanTask } from '@/kanban/types'

export const mockKanbanTasks: KanbanTask[] = [
  { id: '1', title: '设计系统架构文档', participants: ['张三'], status: 'draft', order: 0 },
  { id: '2', title: '编写 API 接口规范', participants: ['赵六'], status: 'draft', order: 1 },
  { id: '3', title: '实现用户认证模块', participants: ['李四', '王五'], status: 'todo', order: 0 },
  { id: '4', title: '数据库表结构设计', participants: ['张三'], status: 'todo', order: 1 },
  {
    id: '5',
    title: '开发看板拖拽交互',
    participants: ['王五', '赵六'],
    status: 'in-progress',
    order: 0,
  },
  {
    id: '6',
    title: '实现 Markdown 实时预览',
    participants: ['李四'],
    status: 'in-progress',
    order: 1,
  },
  {
    id: '7',
    title: '单元测试覆盖率提升',
    participants: ['张三', '李四'],
    status: 'review',
    order: 0,
  },
  { id: '8', title: 'CI/CD 流水线配置', participants: ['赵六'], status: 'review', order: 1 },
  { id: '9', title: '项目初始化与脚手架搭建', participants: ['张三'], status: 'archive', order: 0 },
  {
    id: '10',
    title: '需求分析与原型设计',
    participants: ['李四', '王五'],
    status: 'archive',
    order: 1,
  },
]
