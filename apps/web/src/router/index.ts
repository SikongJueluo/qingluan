import { createRouter, createWebHistory } from 'vue-router'
import HomeView from '@/views/HomeView.vue'
import MarkdownReviewDemoView from '@/views/MarkdownReviewDemoView.vue'

const router = createRouter({
  history: createWebHistory(import.meta.env.BASE_URL),
  routes: [
    {
      path: '/',
      name: 'home',
      component: HomeView,
    },
    {
      path: '/markdown-review',
      name: 'markdown-review',
      component: MarkdownReviewDemoView,
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
