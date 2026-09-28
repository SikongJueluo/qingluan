<template>
  <SidebarProvider>
    <Sidebar collapsible="icon">
      <SidebarHeader>
        <SidebarMenu>
          <SidebarMenuItem>
            <SidebarMenuButton size="lg">
              <div
                class="flex aspect-square size-8 items-center justify-center rounded-lg bg-sidebar-primary text-sidebar-primary-foreground"
              >
                <Bird class="size-4" />
              </div>
              <div class="grid flex-1 text-left text-sm leading-tight">
                <span class="truncate font-semibold">青鸾</span>
                <span class="truncate text-xs">Qingluan</span>
              </div>
            </SidebarMenuButton>
          </SidebarMenuItem>
        </SidebarMenu>
        <!-- tab 切换位于 logo 之下（图标栏模式整体隐藏） -->
        <div
          class="flex rounded-lg border p-0.5 text-xs group-data-[collapsible=icon]:hidden"
          data-testid="sidebar-tabs"
        >
          <button
            v-for="t in tabs"
            :key="t.key"
            class="flex flex-1 items-center justify-center gap-1 rounded-md px-2 py-1.5 transition-colors"
            :class="
              explorer.tab === t.key
                ? 'bg-muted font-medium'
                : 'text-muted-foreground hover:text-foreground'
            "
            @click="explorer.setTab(t.key)"
          >
            <component :is="t.icon" class="size-3.5" />
            {{ t.label }}
          </button>
        </div>
      </SidebarHeader>
      <SidebarContent>
        <!-- 工作区导航（文件 tab 时隐藏；图标栏折叠时恢复显示） -->
        <div
          :class="
            cn(
              'min-h-0',
              explorer.tab === 'files' ? 'hidden group-data-[collapsible=icon]:block' : 'block',
            )
          "
        >
          <SidebarGroup>
            <SidebarGroupLabel>Platform</SidebarGroupLabel>
            <SidebarGroupContent>
              <SidebarMenu>
                <SidebarMenuItem>
                  <SidebarMenuButton as-child tooltip="Home">
                    <router-link to="/">
                      <Home />
                      <span>Home</span>
                    </router-link>
                  </SidebarMenuButton>
                </SidebarMenuItem>
                <SidebarMenuItem>
                  <SidebarMenuButton as-child tooltip="审批">
                    <router-link to="/approvals">
                      <Inbox />
                      <span>审批</span>
                    </router-link>
                  </SidebarMenuButton>
                </SidebarMenuItem>
                <SidebarMenuItem>
                  <SidebarMenuButton as-child tooltip="任务看板">
                    <router-link to="/kanban">
                      <Kanban />
                      <span>任务看板</span>
                    </router-link>
                  </SidebarMenuButton>
                </SidebarMenuItem>
              </SidebarMenu>
            </SidebarGroupContent>
          </SidebarGroup>
        </div>
        <!-- 文件树（review 会话；图标栏模式隐藏） -->
        <div
          v-show="explorer.tab === 'files'"
          class="group-data-[collapsible=icon]:hidden min-h-0 flex-1"
        >
          <ReviewFileTree
            :files="explorer.files"
            :selected="explorer.selectedFile"
            :comment-counts="commentCounts"
            @select="explorer.select($event)"
          />
        </div>
      </SidebarContent>
      <SidebarFooter>
        <SidebarToggle />
      </SidebarFooter>
      <SidebarRail />
    </Sidebar>
    <SidebarInset>
      <div class="flex flex-1 flex-col">
        <router-view />
      </div>
    </SidebarInset>
  </SidebarProvider>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import {
  Sidebar,
  SidebarContent,
  SidebarFooter,
  SidebarGroup,
  SidebarGroupContent,
  SidebarGroupLabel,
  SidebarHeader,
  SidebarInset,
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
  SidebarProvider,
  SidebarRail,
  SidebarToggle,
} from '@/components/ui/sidebar'
import { Home, Bird, Kanban, Inbox, LayoutGrid, FolderTree } from 'lucide-vue-next'
import ReviewFileTree from '@/components/review/ReviewFileTree.vue'
import { useReviewExplorerStore, type SidebarTab } from '@/stores/reviewExplorer'
import { useReviewCommentsStore } from '@/stores/reviewComments'
import { cn } from '@/lib/utils'

const explorer = useReviewExplorerStore()
const comments = useReviewCommentsStore()

const tabs: { key: SidebarTab; label: string; icon: typeof LayoutGrid }[] = [
  { key: 'workspace', label: '工作区', icon: LayoutGrid },
  { key: 'files', label: '文件', icon: FolderTree },
]

/** 树上的评论进度：path → 数量（依赖评论 store，响应式更新）。 */
const commentCounts = computed(() => {
  const map: Record<string, number> = {}
  for (const f of explorer.files) map[f.path] = comments.countForFile(f.path)
  return map
})
</script>

<style scoped></style>
