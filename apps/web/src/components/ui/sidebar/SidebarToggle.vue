<script setup lang="ts">
import { PanelLeftClose, PanelLeftOpen } from 'lucide-vue-next'
import SidebarMenu from './SidebarMenu.vue'
import SidebarMenuButton from './SidebarMenuButton.vue'
import SidebarMenuItem from './SidebarMenuItem.vue'
import { useSidebar } from './utils'

const { state, toggleSidebar } = useSidebar()
</script>

<template>
  <SidebarMenu>
    <SidebarMenuItem>
      <SidebarMenuButton
        data-sidebar="toggle"
        data-slot="sidebar-toggle"
        :tooltip="state === 'expanded' ? '收起侧边栏' : '展开侧边栏'"
        @click="toggleSidebar"
      >
        <PanelLeftClose v-if="state === 'expanded'" class="cn-rtl-flip" />
        <PanelLeftOpen v-else class="cn-rtl-flip" />
        <span>{{ state === 'expanded' ? '收起侧边栏' : '展开侧边栏' }}</span>
      </SidebarMenuButton>
    </SidebarMenuItem>
  </SidebarMenu>
</template>
