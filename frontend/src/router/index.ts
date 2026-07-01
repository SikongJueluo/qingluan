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
  ],
})

export default router
