<script setup lang="ts">
import type { HTMLAttributes } from 'vue'
import { computed } from 'vue'
import { cn } from '@/lib/utils'
import { useSidebar } from './utils'

const props = defineProps<{
  class?: HTMLAttributes['class']
}>()

const {
  state,
  isMobile,
  toggleSidebar,
  sidebarWidth,
  setSidebarWidth,
  resetSidebarWidth,
  resizing,
} = useSidebar()

// Pointer movement below this distance counts as a click (toggle), not a drag.
const DRAG_THRESHOLD_PX = 4

let dragPointerId: number | null = null
let dragStartX = 0
let dragStartWidth = 0
let dragSide: 'left' | 'right' = 'left'
let dragged = false

function onPointerDown(event: PointerEvent) {
  if (state.value === 'collapsed' || isMobile.value) return
  dragPointerId = event.pointerId
  dragStartX = event.clientX
  dragStartWidth = sidebarWidth.value
  dragged = false
  const container = (event.currentTarget as HTMLElement).closest('[data-slot="sidebar-container"]')
  dragSide = container?.getAttribute('data-side') === 'right' ? 'right' : 'left'
  ;(event.currentTarget as HTMLElement).setPointerCapture(event.pointerId)
  event.preventDefault()
}

function onPointerMove(event: PointerEvent) {
  if (dragPointerId !== event.pointerId) return
  const delta = event.clientX - dragStartX
  if (!dragged) {
    if (Math.abs(delta) < DRAG_THRESHOLD_PX) return
    dragged = true
    resizing.value = true
    document.body.classList.add('cursor-col-resize', 'select-none')
  }
  // Dragging inward grows a left sidebar, shrinks a right one.
  const signed = dragSide === 'left' ? delta : -delta
  setSidebarWidth(dragStartWidth + signed)
}

function endDrag() {
  dragPointerId = null
  if (dragged) {
    document.body.classList.remove('cursor-col-resize', 'select-none')
    resizing.value = false
  }
}

function onClick() {
  if (dragged) {
    // A finished drag should not also toggle the sidebar.
    dragged = false
    return
  }
  toggleSidebar()
}

// Collapsed, the rail is click-only: drags are refused, so the cursor and
// hint must not advertise resizing.
const collapsed = computed(() => state.value === 'collapsed')
</script>

<template>
  <button
    data-sidebar="rail"
    data-slot="sidebar-rail"
    :aria-label="collapsed ? '展开侧边栏' : '收起侧边栏'"
    :tabindex="-1"
    :title="collapsed ? '单击展开侧边栏' : '拖动调整宽度；双击恢复默认；单击收起'"
    :class="
      cn(
        'hover:after:bg-sidebar-border absolute inset-y-0 z-20 hidden w-4 transition-all ease-linear group-data-[side=left]:-right-4 group-data-[side=right]:left-0 after:absolute after:inset-y-0 after:start-1/2 after:w-[2px] sm:flex ltr:-translate-x-1/2 rtl:-translate-x-1/2',
        '[[data-side=left][data-state=expanded]_&]:cursor-w-resize [[data-side=right][data-state=expanded]_&]:cursor-e-resize',
        '[[data-state=collapsed]_&]:cursor-pointer',
        'group-data-[collapsible=offcanvas]:translate-x-0 group-data-[collapsible=offcanvas]:after:left-full hover:group-data-[collapsible=offcanvas]:bg-sidebar',
        '[[data-side=left][data-collapsible=offcanvas]_&]:-right-2',
        '[[data-side=right][data-collapsible=offcanvas]_&]:-left-2',
        props.class,
      )
    "
    @pointerdown="onPointerDown"
    @pointermove="onPointerMove"
    @pointerup="endDrag"
    @pointercancel="endDrag"
    @click="onClick"
    @dblclick="resetSidebarWidth"
  >
    <slot />
  </button>
</template>
