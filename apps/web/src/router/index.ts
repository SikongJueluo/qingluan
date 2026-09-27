import { createRouter, createWebHistory } from 'vue-router'
import ApprovalsView from '@/views/ApprovalsView.vue'
import HomeView from '@/views/HomeView.vue'
import MarkdownReviewView from '@/views/MarkdownReviewView.vue'

const router = createRouter({
  history: createWebHistory(import.meta.env.BASE_URL),
  routes: [
    {
      path: '/',
      name: 'home',
      component: HomeView,
    },
    {
      path: '/approvals',
      name: 'approvals',
      component: ApprovalsView,
    },
    {
      // 不在侧边栏露出：由审批收件箱「详情」进入（同 /review/:id 模式）
      path: '/markdown-review/:id',
      name: 'markdown-review',
      component: MarkdownReviewView,
    },
    {
      path: '/kanban',
      name: 'kanban',
      component: () => import('@/views/KanbanBoardView.vue'),
    },
    {
      path: '/review/:id',
      name: 'code-review',
      component: () => import('@/views/CodeReviewView.vue'),
    },
  ],
})

export default router
