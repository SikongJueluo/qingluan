<!--
  Review 文件树（GitHub PR 风格）：目录在前文件在后、可折叠，
  文件行右侧带 +N/−M 与评论数，选中行高亮。
  基于 reka-ui Tree 原语（键盘漫游可达），树构建见 ./file-tree.ts。
-->
<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { TreeItem, TreeRoot } from 'reka-ui'
import {
  ChevronRight,
  FileCode,
  Folder,
  FolderOpen,
  ListTree,
  MessageSquare,
} from 'lucide-vue-next'
import { Button } from '@/components/ui/button'
import type { ChangedFileMeta } from '@/components/code-review/types'
import { allDirKeys, buildFileTree, type TreeNode } from './file-tree'

const props = defineProps<{
  files: ChangedFileMeta[]
  /** null = 总览模式。 */
  selected: string | null
  /** path → 评论数（审查进度）。 */
  commentCounts?: Record<string, number>
}>()

const emit = defineEmits<{ select: [path: string | null] }>()

const tree = computed(() => buildFileTree(props.files))

// 默认全展开：diff 路径一般不深；切换会话时重置。
const expandedKeys = ref<string[]>([])
watch(
  tree,
  (nodes) => {
    expandedKeys.value = allDirKeys(nodes)
  },
  { immediate: true },
)

const selectedNode = computed<TreeNode | undefined>(() => {
  if (!props.selected) return undefined
  return findNode(tree.value, props.selected)
})

function findNode(nodes: TreeNode[], key: string): TreeNode | undefined {
  for (const n of nodes) {
    if (n.key === key) return n
    if (n.dir) {
      const hit = findNode(n.children ?? [], key)
      if (hit) return hit
    }
  }
  return undefined
}

function onModelUpdate(value: unknown) {
  const node = value as TreeNode | undefined
  // 只认文件节点；目录行的内置点击（select+toggle）在这里被过滤。
  if (node && !node.dir) emit('select', node.key)
}
</script>

<template>
  <div class="flex h-full min-h-0 flex-col gap-1">
    <!-- 总览入口：回到全部文件堆叠视图 -->
    <div class="px-1">
      <Button
        variant="ghost"
        size="sm"
        class="h-7 w-full justify-start px-2 text-sm"
        :class="{ 'bg-accent text-accent-foreground': selected === null }"
        title="全部文件与总览统计"
        @click="emit('select', null)"
      >
        <ListTree class="size-3.5" />
        总览
        <span class="ml-auto text-xs tabular-nums text-muted-foreground">{{ files.length }}</span>
      </Button>
    </div>

    <TreeRoot
      class="min-h-0 flex-1 overflow-y-auto pb-2"
      :items="tree"
      :get-key="(n: TreeNode) => n.key"
      :get-children="(n: TreeNode) => n.children"
      :model-value="selectedNode"
      :expanded="expandedKeys"
      selection-behavior="replace"
      @update:expanded="expandedKeys = $event"
      @update:model-value="onModelUpdate"
    >
      <template #default="{ flattenItems }">
        <TreeItem v-for="item in flattenItems" :key="item._id" v-bind="item.bind" as-child>
          <template #default="{ isExpanded, isSelected }">
            <!-- 点击交互交给 TreeItem 内置行为（select + 目录 toggle）。 -->
            <div
              class="flex h-7 cursor-pointer items-center gap-1 rounded-md pr-2 text-sm transition-colors hover:bg-accent/60 group-data-[collapsible=icon]:hidden"
              :class="isSelected && !item.value.dir ? 'bg-accent text-accent-foreground' : ''"
              :style="{ marginInlineStart: `${item.level * 14 + 4}px` }"
              :title="item.value.key"
            >
              <ChevronRight
                class="size-3.5 shrink-0 text-muted-foreground transition-transform"
                :class="isExpanded ? 'rotate-90' : ''"
              />
              <FolderOpen
                v-if="item.value.dir && isExpanded"
                class="size-4 shrink-0 text-primary/70"
              />
              <Folder v-else-if="item.value.dir" class="size-4 shrink-0 text-primary/70" />
              <FileCode v-else class="size-4 shrink-0 text-muted-foreground" />
              <span class="truncate">{{ item.value.name }}</span>
              <template v-if="!item.value.dir && item.value.meta">
                <span class="ml-auto flex shrink-0 items-center gap-1.5 text-xs">
                  <span
                    v-if="(commentCounts?.[item.value.key] ?? 0) > 0"
                    class="flex items-center gap-0.5 text-muted-foreground"
                    :title="`${commentCounts?.[item.value.key]} 条评论`"
                  >
                    <MessageSquare class="size-3" />
                    {{ commentCounts?.[item.value.key] }}
                  </span>
                  <span class="tabular-nums">
                    <span class="text-green-600 dark:text-green-400"
                      >+{{ item.value.meta.additions }}</span
                    >
                    <span class="text-red-600 dark:text-red-400">
                      −{{ item.value.meta.deletions }}</span
                    >
                  </span>
                </span>
              </template>
            </div>
          </template>
        </TreeItem>
      </template>
    </TreeRoot>

    <div
      v-if="!files.length"
      class="flex flex-col items-center gap-1 px-4 py-10 text-center text-xs text-muted-foreground"
    >
      <p>进入 review 会话后</p>
      <p>这里显示变更文件树</p>
    </div>
  </div>
</template>
